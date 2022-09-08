#[cfg(target_os = "linux")]
extern crate alsa;
extern crate camillalib;
extern crate clap;
#[cfg(feature = "FFTW")]
extern crate fftw;
extern crate lazy_static;
#[cfg(feature = "pulse-backend")]
extern crate libpulse_binding as pulse;
#[cfg(feature = "pulse-backend")]
extern crate libpulse_simple_binding as psimple;
extern crate rand;
extern crate rand_distr;
#[cfg(not(feature = "FFTW"))]
extern crate realfft;
extern crate rubato;
extern crate serde;
extern crate serde_with;
extern crate signal_hook;
#[cfg(feature = "websocket")]
extern crate tungstenite;

extern crate flexi_logger;
extern crate time;
#[macro_use]
extern crate log;

use clap::{crate_authors, crate_description, crate_version, App, AppSettings, Arg};
use crossbeam::select;
use nix::libc::EXIT_FAILURE;
use std::env;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex, RwLock};

use flexi_logger::DeferredNow;
use log::Record;
use time::format_description;

use camillalib::{ControllerMessage, Res};

use camillalib::audiodevice;
use camillalib::config;
use camillalib::countertimer;
use camillalib::processing;
#[cfg(feature = "websocket")]
use camillalib::socketserver;
#[cfg(feature = "websocket")]
use std::net::IpAddr;

use camillalib::{
    list_supported_devices, CaptureStatus, CommandMessage, ExitState, PlaybackStatus,
    ProcessingParameters, ProcessingState, ProcessingStatus, StatusMessage, StatusStructs,
    StopReason,
};

const EXIT_OK: i32 = 0; // All ok

// Time format string for logger
const TS_S: &str = "[year]-[month]-[day] [hour]:[minute]:[second].[subsecond digits:6]";
lazy_static::lazy_static! {
    static ref TS: Vec<format_description::FormatItem<'static>>
        = format_description::parse(TS_S).unwrap(/*ok*/);
}

// Customized version of `colored_opt_format` from flexi_logger.
fn custom_colored_logger_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    let level = record.level();
    write!(
        w,
        "{} {:<5} [{}:{}] {}",
        now.now()
            .format(&TS)
            .unwrap_or_else(|_| "Timestamping failed".to_string()),
        flexi_logger::style(level).paint(level.to_string()),
        record.file().unwrap_or("<unnamed>"),
        record.line().unwrap_or(0),
        &record.args()
    )
}

// Customized version of `opt_format` from flexi_logger.
pub fn custom_logger_format(
    w: &mut dyn std::io::Write,
    now: &mut DeferredNow,
    record: &Record,
) -> Result<(), std::io::Error> {
    write!(
        w,
        "{} {:<5} [{}:{}] {}",
        now.now()
            .format(&TS)
            .unwrap_or_else(|_| "Timestamping failed".to_string()),
        record.level(),
        record.file().unwrap_or("<unnamed>"),
        record.line().unwrap_or(0),
        &record.args()
    )
}

fn run(
    status_structs: StatusStructs,
    active_config: Arc<Mutex<Option<config::Configuration>>>,
    rx_ctrl: crossbeam::channel::Receiver<ControllerMessage>,
) -> Res<ExitState> {
    status_structs.capture.write().unwrap().state = ProcessingState::Starting;
    let mut is_starting = true;
    let conf = active_config.lock().unwrap().clone().unwrap();
    let (tx_status, rx_status) = crossbeam::channel::unbounded::<StatusMessage>();

    let (tx_pb, rx_pb) = mpsc::sync_channel(conf.devices.queuelimit);
    let (tx_cap, rx_cap) = mpsc::sync_channel(conf.devices.queuelimit);

    let (tx_command_cap, rx_command_cap) = mpsc::channel();
    let (tx_pipeconf, rx_pipeconf) = mpsc::channel();

    let barrier = Arc::new(Barrier::new(4));
    let barrier_pb = barrier.clone();
    let barrier_cap = barrier.clone();
    let barrier_proc = barrier.clone();

    let conf_pb = conf.clone();
    let conf_cap = conf.clone();
    let conf_proc = conf.clone();

    // Processing thread
    processing::run_processing(
        conf_proc,
        barrier_proc,
        tx_pb,
        rx_cap,
        rx_pipeconf,
        status_structs.processing,
    );

    // Playback thread
    let mut playback_dev = audiodevice::get_playback_device(conf_pb.devices);
    let pb_handle = playback_dev
        .start(
            rx_pb,
            barrier_pb,
            tx_status.clone(),
            status_structs.playback,
        )
        .unwrap();

    let used_channels = config::get_used_capture_channels(&conf);
    debug!("Using channels {:?}", used_channels);
    status_structs.capture.write().unwrap().used_channels = used_channels;

    // Capture thread
    let mut capture_dev = audiodevice::get_capture_device(conf_cap.devices);
    let cap_handle = capture_dev
        .start(
            tx_cap,
            barrier_cap,
            tx_status.clone(),
            rx_command_cap,
            status_structs.capture.clone(),
        )
        .unwrap();

    let mut pb_ready = false;
    let mut cap_ready = false;
    loop {
        select! {
            recv(rx_ctrl) -> msg => {
                match msg {
                    Ok(ControllerMessage::ConfigChanged(new_conf)) => {
                        let comp = config::config_diff(&conf, &new_conf);
                        match comp {
                            config::ConfigChange::Pipeline
                            | config::ConfigChange::MixerParameters
                            | config::ConfigChange::FilterParameters { .. } => {
                                tx_pipeconf.send((comp, new_conf.clone())).unwrap();
                                let used_channels = config::get_used_capture_channels(&new_conf);
                                *active_config.lock().unwrap() = Some(new_conf);
                                debug!("Using channels {:?}", used_channels);
                                status_structs.capture.write().unwrap().used_channels = used_channels;
                                debug!("Sent changes to pipeline");
                            }
                            config::ConfigChange::Devices => {
                                debug!("Devices changed, restart required.");
                                if tx_command_cap.send(CommandMessage::Exit).is_err() {
                                    debug!("Capture thread has already exited");
                                }
                                trace!("Wait for pb..");
                                pb_handle.join().unwrap();
                                trace!("Wait for cap..");
                                cap_handle.join().unwrap();
                                trace!("All threads stopped, returning");
                                *active_config.lock().unwrap() = Some(new_conf);
                                return Ok(ExitState::Restart);
                            }
                            config::ConfigChange::None => {
                                debug!("No changes in config.");
                            }
                        };
                    }
                    Ok(ControllerMessage::Stop) => {
                        if is_starting {
                            barrier.wait();
                        }
                        if tx_command_cap.send(CommandMessage::Exit).is_err() {
                            debug!("Capture thread has already exited");
                        }
                        trace!("Wait for pb..");
                        pb_handle.join().unwrap();
                        trace!("Wait for cap..");
                        cap_handle.join().unwrap();
                        trace!("All threads stopped, stopping");
                        *active_config.lock().unwrap() = None;
                        return Ok(ExitState::Restart);
                    }
                    Ok(ControllerMessage::Exit) => {
                        if is_starting {
                            barrier.wait();
                        }
                        if tx_command_cap.send(CommandMessage::Exit).is_err() {
                            debug!("Capture thread has already exited");
                        }
                        trace!("Wait for pb..");
                        pb_handle.join().unwrap();
                        trace!("Wait for cap..");
                        cap_handle.join().unwrap();
                        trace!("All threads stopped, stopping");
                        return Ok(ExitState::Exit);
                    }
                    Err(e) => {
                        return Err(Box::new(e));
                    }
                }
            }
            recv(rx_status) -> msg => {
                match msg {
                    Ok(StatusMessage::PlaybackReady) => {
                        debug!("Playback thread ready to start");
                        pb_ready = true;
                        if cap_ready {
                            debug!("Both capture and playback ready, release barrier");
                            barrier.wait();
                            debug!("Supervisor loop starts now!");
                            is_starting = false;
                        }
                    }
                    Ok(StatusMessage::CaptureReady) => {
                        debug!("Capture thread ready to start");
                        cap_ready = true;
                        if pb_ready {
                            debug!("Both capture and playback ready, release barrier");
                            barrier.wait();
                            debug!("Supervisor loop starts now!");
                            is_starting = false;
                            status_structs.status.write().unwrap().stop_reason = StopReason::None;
                        }
                    }
                    Ok(StatusMessage::PlaybackError(message)) => {
                        error!("Playback error: {}", message);
                        if tx_command_cap.send(CommandMessage::Exit).is_err() {
                            debug!("Capture thread has already exited");
                        }
                        if is_starting {
                            debug!("Error while starting, release barrier");
                            barrier.wait();
                        }
                        debug!("Wait for capture thread to exit..");
                        status_structs.status.write().unwrap().stop_reason =
                            StopReason::PlaybackError(message);
                        cap_handle.join().unwrap();
                        trace!("All threads stopped, returning");
                        return Ok(ExitState::Restart);
                    }
                    Ok(StatusMessage::CaptureError(message)) => {
                        error!("Capture error: {}", message);
                        if is_starting {
                            debug!("Error while starting, release barrier");
                            barrier.wait();
                        }
                        debug!("Wait for playback thread to exit..");
                        status_structs.status.write().unwrap().stop_reason =
                            StopReason::CaptureError(message);
                        pb_handle.join().unwrap();
                        trace!("All threads stopped, returning");
                        return Ok(ExitState::Restart);
                    }
                    Ok(StatusMessage::PlaybackFormatChange(rate)) => {
                        error!("Playback stopped due to external format change");
                        if tx_command_cap.send(CommandMessage::Exit).is_err() {
                            debug!("Capture thread has already exited");
                        }
                        if is_starting {
                            debug!("Error while starting, release barrier");
                            barrier.wait();
                        }
                        debug!("Wait for capture thread to exit..");
                        status_structs.status.write().unwrap().stop_reason =
                            StopReason::PlaybackFormatChange(rate);
                        cap_handle.join().unwrap();
                        trace!("All threads stopped, returning");
                        return Ok(ExitState::Restart);
                    }
                    Ok(StatusMessage::CaptureFormatChange(rate)) => {
                        error!("Capture stopped due to external format change");
                        if is_starting {
                            debug!("Error while starting, release barrier");
                            barrier.wait();
                        }
                        debug!("Wait for playback thread to exit..");
                        status_structs.status.write().unwrap().stop_reason =
                            StopReason::CaptureFormatChange(rate);
                        pb_handle.join().unwrap();
                        trace!("All threads stopped, returning");
                        return Ok(ExitState::Restart);
                    }
                    Ok(StatusMessage::PlaybackDone) => {
                        info!("Playback finished");
                        let mut stat = status_structs.status.write().unwrap();
                        if stat.stop_reason == StopReason::None {
                            stat.stop_reason = StopReason::Done;
                        }
                        trace!("All threads stopped, returning");
                        return Ok(ExitState::Restart);
                    }
                    Ok(StatusMessage::CaptureDone) => {
                        info!("Capture finished");
                    }
                    Ok(StatusMessage::SetSpeed(speed)) => {
                        debug!("SetSpeed message received");
                        if tx_command_cap
                            .send(CommandMessage::SetSpeed { speed })
                            .is_err()
                        {
                            debug!("Capture thread has already exited");
                        }
                    }
                    Err(_) => {
                        return Ok(ExitState::Restart);
                    }
                }
            }
        }
    }
}

fn main_process() -> i32 {
    let mut features = Vec::new();
    if cfg!(feature = "pulse-backend") {
        features.push("pulse-backend");
    }
    if cfg!(feature = "cpal-backend") {
        features.push("cpal-backend");
    }
    if cfg!(feature = "jack-backend") {
        features.push("jack-backend");
    }
    if cfg!(feature = "websocket") {
        features.push("websocket");
    }
    if cfg!(feature = "secure-websocket") {
        features.push("secure-websocket");
    }
    if cfg!(feature = "FFTW") {
        features.push("FFTW");
    }
    if cfg!(feature = "32bit") {
        features.push("32bit");
    }
    if cfg!(feature = "neon") {
        features.push("neon");
    }
    if cfg!(feature = "debug") {
        features.push("debug");
    }
    let featurelist = format!("Built with features: {}", features.join(", "));

    let (pb_types, cap_types) = list_supported_devices();
    let playback_types = format!("Playback: {}", pb_types.join(", "));
    let capture_types = format!("Capture: {}", cap_types.join(", "));

    let longabout = format!(
        "{}\n\n{}\n\nSupported device types:\n{}\n{}",
        crate_description!(),
        featurelist,
        capture_types,
        playback_types
    );

    let clapapp = App::new("CamillaDSP")
        .version(crate_version!())
        .about(longabout.as_str())
        .author(crate_authors!())
        .setting(AppSettings::ArgRequiredElseHelp)
        .arg(
            Arg::with_name("configfile")
                .help("The configuration file to use")
                .index(1)
                //.required(true),
                .required_unless("wait"),
        )
        .arg(
            Arg::with_name("check")
                .help("Check config file and exit")
                .short("c")
                .long("check")
                .requires("configfile"),
        )
        .arg(
            Arg::with_name("verbosity")
                .short("v")
                .multiple(true)
                .help("Increase message verbosity"),
        )
        .arg(
            Arg::with_name("loglevel")
                .short("l")
                .long("loglevel")
                .display_order(100)
                .takes_value(true)
                .possible_value("trace")
                .possible_value("debug")
                .possible_value("info")
                .possible_value("warn")
                .possible_value("error")
                .possible_value("off")
                .help("Set log level")
                .conflicts_with("verbosity"),
        )
        .arg(
            Arg::with_name("logfile")
                .short("o")
                .long("logfile")
                .display_order(100)
                .takes_value(true)
                .help("Write logs to file"),
        )
        .arg(
            Arg::with_name("gain")
                .help("Set initial gain in dB for Volume and Loudness filters")
                .short("g")
                .long("gain")
                .display_order(200)
                .takes_value(true)
                .validator(|v: String| -> Result<(), String> {
                    if let Ok(gain) = v.parse::<f32>() {
                        if (-120.0..=20.0).contains(&gain) {
                            return Ok(());
                        }
                    }
                    Err(String::from("Must be a number between -120 and +20"))
                }),
        )
        .arg(
            Arg::with_name("mute")
                .help("Start with Volume and Loudness filters muted")
                .short("m")
                .long("mute")
                .display_order(200),
        )
        .arg(
            Arg::with_name("samplerate")
                .help("Override samplerate in config")
                .short("r")
                .long("samplerate")
                .display_order(300)
                .takes_value(true)
                .validator(|v: String| -> Result<(), String> {
                    if let Ok(rate) = v.parse::<usize>() {
                        if rate > 0 {
                            return Ok(());
                        }
                    }
                    Err(String::from("Must be an integer > 0"))
                }),
        )
        .arg(
            Arg::with_name("channels")
                .help("Override number of channels of capture device in config")
                .short("n")
                .long("channels")
                .display_order(300)
                .takes_value(true)
                .validator(|v: String| -> Result<(), String> {
                    if let Ok(rate) = v.parse::<usize>() {
                        if rate > 0 {
                            return Ok(());
                        }
                    }
                    Err(String::from("Must be an integer > 0"))
                }),
        )
        .arg(
            Arg::with_name("extra_samples")
                .help("Override number of extra samples in config")
                .short("e")
                .long("extra_samples")
                .display_order(300)
                .takes_value(true)
                .validator(|v: String| -> Result<(), String> {
                    if let Ok(_samples) = v.parse::<usize>() {
                        return Ok(());
                    }
                    Err(String::from("Must be an integer > 0"))
                }),
        )
        .arg(
            Arg::with_name("format")
                .short("f")
                .long("format")
                .display_order(310)
                .takes_value(true)
                .possible_value("S16LE")
                .possible_value("S24LE")
                .possible_value("S24LE3")
                .possible_value("S32LE")
                .possible_value("FLOAT32LE")
                .possible_value("FLOAT64LE")
                .help("Override sample format of capture device in config"),
        );
    #[cfg(feature = "websocket")]
    let clapapp = clapapp
        .arg(
            Arg::with_name("port")
                .help("Port for websocket server")
                .short("p")
                .long("port")
                .display_order(200)
                .takes_value(true)
                .validator(|v: String| -> Result<(), String> {
                    if let Ok(port) = v.parse::<usize>() {
                        if port > 0 && port < 65535 {
                            return Ok(());
                        }
                    }
                    Err(String::from("Must be an integer between 0 and 65535"))
                }),
        )
        .arg(
            Arg::with_name("address")
                .help("IP address to bind websocket server to")
                .short("a")
                .long("address")
                .display_order(200)
                .takes_value(true)
                .requires("port")
                .validator(|val: String| -> Result<(), String> {
                    if val.parse::<IpAddr>().is_ok() {
                        return Ok(());
                    }
                    Err(String::from("Must be a valid IP address"))
                }),
        )
        .arg(
            Arg::with_name("wait")
                .short("w")
                .long("wait")
                .help("Wait for config from websocket")
                .requires("port"),
        );
    #[cfg(feature = "secure-websocket")]
    let clapapp = clapapp
        .arg(
            Arg::with_name("cert")
                .long("cert")
                .takes_value(true)
                .help("Path to .pfx/.p12 certificate file")
                .requires("port"),
        )
        .arg(
            Arg::with_name("pass")
                .long("pass")
                .takes_value(true)
                .help("Password for .pfx/.p12 certificate file")
                .requires("port"),
        );
    let matches = clapapp.get_matches();

    let mut loglevel = match matches.occurrences_of("verbosity") {
        0 => "info",
        1 => "debug",
        2 => "trace",
        _ => "trace",
    };

    if let Some(level) = matches.value_of("loglevel") {
        loglevel = level;
    }

    let _logger = if let Some(logfile) = matches.value_of("logfile") {
        let mut path = PathBuf::from(logfile);
        if !path.is_absolute() {
            let mut fullpath = std::env::current_dir().unwrap();
            fullpath.push(path);
            path = fullpath;
        }
        flexi_logger::Logger::try_with_str(loglevel)
            .unwrap()
            .format(custom_logger_format)
            .log_to_file(flexi_logger::FileSpec::try_from(path).unwrap())
            .write_mode(flexi_logger::WriteMode::Async)
            .start()
            .unwrap()
    } else {
        flexi_logger::Logger::try_with_str(loglevel)
            .unwrap()
            .format(custom_colored_logger_format)
            .set_palette("196;208;-;27;8".to_string())
            .log_to_stderr()
            .write_mode(flexi_logger::WriteMode::Async)
            .start()
            .unwrap()
    };
    info!("CamillaDSP version {} (Customized for SmartCross)", crate_version!());
    info!(
        "Running on {}, {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    // logging examples
    //trace!("trace message"); //with -vv
    //debug!("debug message"); //with -v
    //info!("info message");
    //warn!("warn message");
    //error!("error message");

    let (tx_ctrl, rx_ctrl) = crossbeam::channel::unbounded::<ControllerMessage>();

    let capture_status = Arc::new(RwLock::new(CaptureStatus {
        measured_samplerate: 0,
        update_interval: 1000,
        signal_range: 0.0,
        rate_adjust: 0.0,
        state: ProcessingState::Inactive,
        signal_rms: countertimer::ValueHistory::new(1024, 2),
        signal_peak: countertimer::ValueHistory::new(1024, 2),
        used_channels: Vec::new(),
    }));
    let playback_status = Arc::new(RwLock::new(PlaybackStatus {
        buffer_level: 0,
        clipped_samples: 0,
        update_interval: 1000,
        signal_rms: countertimer::ValueHistory::new(1024, 2),
        signal_peak: countertimer::ValueHistory::new(1024, 2),
    }));
    let processing_status = Arc::new(RwLock::new(ProcessingParameters {
        volume: 0.0,
        mute: false,
    }));
    let status = Arc::new(RwLock::new(ProcessingStatus {
        stop_reason: StopReason::None,
    }));

    let status_structs = StatusStructs {
        capture: capture_status.clone(),
        playback: playback_status.clone(),
        processing: processing_status.clone(),
        status: status.clone(),
    };

    #[cfg(feature = "websocket")]
    {
        if let Some(port_str) = matches.value_of("port") {
            let serveraddress = matches.value_of("address").unwrap_or("127.0.0.1");
            let serverport = port_str.parse::<usize>().unwrap();
            let shared_data = socketserver::SharedData {
                command_sender: tx_ctrl.clone(),
                capture_status,
                playback_status,
                processing_status,
                status,
            };
            let server_params = socketserver::ServerParameters {
                port: serverport,
                address: serveraddress,
                #[cfg(feature = "secure-websocket")]
                cert_file: matches.value_of("cert"),
                #[cfg(feature = "secure-websocket")]
                cert_pass: matches.value_of("pass"),
            };
            socketserver::start_server(server_params, shared_data);
        }
    }

    let active_config = Arc::new(Mutex::<Option<config::Configuration>>::new(None));

    loop {
        if active_config.lock().unwrap().is_some() {
            info!("Got config, starting!");
            let exitstatus = run(status_structs.clone(), active_config.clone(), rx_ctrl.clone());
            match exitstatus {
                Ok(ExitState::Restart) => {
                    continue;
                }
                Ok(ExitState::Exit) => {
                    return EXIT_OK;
                }
                Err(e) => {
                    warn!("Error: {}", e);
                }
            }
        } else {
            info!("Wait for config");
            drop(sd_notify::notify(true, &[sd_notify::NotifyState::Ready]));
    
            let msg = rx_ctrl.recv();
            match msg {
                Ok(ControllerMessage::ConfigChanged(new_conf)) => {
                    *active_config.lock().unwrap() = Some(new_conf);
                }
                Ok(ControllerMessage::Stop) => {
                    *active_config.lock().unwrap() = None;
                }
                Ok(ControllerMessage::Exit) => {
                    return EXIT_OK;
                }
                Err(e) => {
                    warn!("Error {}", e);
                    return EXIT_FAILURE;
                }
            }
        }
    }

    /*
    loop {
        debug!("Wait for config");
        while new_config.lock().unwrap().is_none() {
            if !wait {
                debug!("No config and not in wait mode, exiting!");
                return EXIT_OK;
            }
            trace!("waiting...");
            if signal_exit.load(Ordering::Relaxed) == ExitRequest::EXIT {
                // exit requested
                return EXIT_OK;
            }
            thread::sleep(delay);
        }
        debug!("Config ready");
        let exitstatus = run(
            signal_exit.clone(),
            active_config.clone(),
            active_config_path.clone(),
            new_config.clone(),
            previous_config.clone(),
            status_structs.clone(),
        );
        match exitstatus {
            Err(e) => {
                *active_config.lock().unwrap() = None;
                error!("({}) {}", e.to_string(), e);
                if !wait {
                    return EXIT_PROCESSING_ERROR;
                }
            }
            Ok(ExitState::Exit) => {
                debug!("Exiting");
                *active_config.lock().unwrap() = None;
                return EXIT_OK;
            }
            Ok(ExitState::Restart) => {
                *active_config.lock().unwrap() = None;
                debug!("Restarting with new config");
            }
        };
    }
    */
}

fn main() {
    std::process::exit(main_process());
}
