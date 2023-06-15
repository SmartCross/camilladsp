use clap::crate_version;
#[cfg(feature = "secure-websocket")]
use native_tls::{Identity, TlsAcceptor, TlsStream};
use serde::{Deserialize, Serialize};
#[cfg(feature = "secure-websocket")]
use std::fs::File;
#[cfg(feature = "secure-websocket")]
use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};
use tungstenite::accept;
use tungstenite::Message;
use tungstenite::WebSocket;

use crate::{config, ControllerMessage};
use crate::helpers::linear_to_db;
use crate::ProcessingState;
use crate::Res;
use crate::{
    list_supported_devices, CaptureStatus, PlaybackStatus, ProcessingParameters, ProcessingStatus,
    StopReason,
};

#[derive(Debug, Clone)]
pub struct SharedData {
    pub command_sender: crossbeam::channel::Sender<ControllerMessage>,
    pub capture_status: Arc<RwLock<CaptureStatus>>,
    pub playback_status: Arc<RwLock<PlaybackStatus>>,
    pub processing_status: Arc<RwLock<ProcessingParameters>>,
    pub status: Arc<RwLock<ProcessingStatus>>,
}

#[derive(Debug, Clone)]
pub struct LocalData {
    pub last_cap_rms_time: Instant,
    pub last_cap_peak_time: Instant,
    pub last_pb_rms_time: Instant,
    pub last_pb_peak_time: Instant,
}

#[derive(Debug, Clone)]
pub struct ServerParameters<'a> {
    pub address: &'a str,
    pub port: usize,
    #[cfg(feature = "secure-websocket")]
    pub cert_file: Option<&'a str>,
    #[cfg(feature = "secure-websocket")]
    pub cert_pass: Option<&'a str>,
}

#[derive(Debug, PartialEq, Deserialize)]
enum WsCommand {
    // SetConfigName(String),
    // SetConfig(String),
    SetConfigJson(String),
    // Reload,
    // GetConfig,
    // GetPreviousConfig,
    // ReadConfig(String),
    // ReadConfigFile(String),
    ValidateConfig(String),
    // GetConfigJson,
    // GetConfigName,
    GetSignalRange,
    GetCaptureSignalRms,
    GetCaptureSignalRmsSince(f32),
    GetCaptureSignalRmsSinceLast,
    GetCaptureSignalPeak,
    GetCaptureSignalPeakSince(f32),
    GetCaptureSignalPeakSinceLast,
    GetPlaybackSignalRms,
    GetPlaybackSignalRmsSince(f32),
    GetPlaybackSignalRmsSinceLast,
    GetPlaybackSignalPeak,
    GetPlaybackSignalPeakSince(f32),
    GetPlaybackSignalPeakSinceLast,
    GetSignalLevels,
    GetSignalLevelsSince(f32),
    GetSignalLevelsSinceLast,
    GetSignalPeaksSinceStart,
    ResetSignalPeaksSinceStart,
    GetCaptureRate,
    GetUpdateInterval,
    SetUpdateInterval(usize),
    GetVolume,
    SetVolume(f32),
    AdjustVolume(f32),
    GetMute,
    SetMute(bool),
    ToggleMute,
    GetVersion,
    GetState,
    GetStopReason,
    GetRateAdjust,
    GetClippedSamples,
    GetBufferLevel,
    GetSupportedDeviceTypes,
    Exit,
    Stop,
    None,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
enum WsResult {
    Ok,
    Error,
}

#[derive(Debug, PartialEq, Serialize)]
struct AllLevels {
    playback_rms: Vec<f32>,
    playback_peak: Vec<f32>,
    capture_rms: Vec<f32>,
    capture_peak: Vec<f32>,
}

#[derive(Debug, PartialEq, Serialize)]
struct PbCapLevels {
    playback: Vec<f32>,
    capture: Vec<f32>,
}

#[derive(Debug, PartialEq, Serialize)]
enum WsReply {
    SetConfigJson {
        result: WsResult,
    },
    ValidateConfig {
        result: WsResult,
        value: String,
    },
    GetSignalRange {
        result: WsResult,
        value: f32,
    },
    GetPlaybackSignalRms {
        result: WsResult,
        value: Vec<f32>,
    },
    GetPlaybackSignalRmsSince {
        result: WsResult,
        value: Vec<f32>,
    },
    GetPlaybackSignalRmsSinceLast {
        result: WsResult,
        value: Vec<f32>,
    },
    GetPlaybackSignalPeak {
        result: WsResult,
        value: Vec<f32>,
    },
    GetPlaybackSignalPeakSince {
        result: WsResult,
        value: Vec<f32>,
    },
    GetPlaybackSignalPeakSinceLast {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalRms {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalRmsSince {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalRmsSinceLast {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalPeak {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalPeakSince {
        result: WsResult,
        value: Vec<f32>,
    },
    GetCaptureSignalPeakSinceLast {
        result: WsResult,
        value: Vec<f32>,
    },
    GetSignalLevels {
        result: WsResult,
        value: AllLevels,
    },
    GetSignalLevelsSince {
        result: WsResult,
        value: AllLevels,
    },
    GetSignalLevelsSinceLast {
        result: WsResult,
        value: AllLevels,
    },
    GetSignalPeaksSinceStart {
        result: WsResult,
        value: PbCapLevels,
    },
    ResetSignalPeaksSinceStart {
        result: WsResult,
    },
    GetCaptureRate {
        result: WsResult,
        value: usize,
    },
    GetUpdateInterval {
        result: WsResult,
        value: usize,
    },
    SetUpdateInterval {
        result: WsResult,
    },
    SetVolume {
        result: WsResult,
    },
    GetVolume {
        result: WsResult,
        value: f32,
    },
    AdjustVolume {
        result: WsResult,
        value: f32,
    },
    SetMute {
        result: WsResult,
    },
    GetMute {
        result: WsResult,
        value: bool,
    },
    ToggleMute {
        result: WsResult,
        value: bool,
    },
    GetVersion {
        result: WsResult,
        value: String,
    },
    GetState {
        result: WsResult,
        value: ProcessingState,
    },
    GetStopReason {
        result: WsResult,
        value: StopReason,
    },
    GetRateAdjust {
        result: WsResult,
        value: f32,
    },
    GetBufferLevel {
        result: WsResult,
        value: usize,
    },
    GetClippedSamples {
        result: WsResult,
        value: usize,
    },
    GetSupportedDeviceTypes {
        result: WsResult,
        value: (Vec<String>, Vec<String>),
    },
    Exit {
        result: WsResult,
    },
    Stop {
        result: WsResult,
    },
    Invalid {
        error: String,
    },
}

fn parse_command(cmd: Message) -> Res<WsCommand> {
    match cmd {
        Message::Text(command_str) => {
            let command = serde_json::from_str::<WsCommand>(&command_str)?;
            Ok(command)
        }
        _ => Ok(WsCommand::None),
    }
}

#[cfg(feature = "secure-websocket")]
fn make_acceptor_with_cert(cert: &str, key: &str) -> Res<Arc<TlsAcceptor>> {
    let mut file = File::open(cert)?;
    let mut identity = vec![];
    file.read_to_end(&mut identity)?;
    let identity = Identity::from_pkcs12(&identity, key)?;
    let acceptor = TlsAcceptor::new(identity)?;
    Ok(Arc::new(acceptor))
}

#[cfg(feature = "secure-websocket")]
fn make_acceptor(cert_file: &Option<&str>, cert_key: &Option<&str>) -> Option<Arc<TlsAcceptor>> {
    if let (Some(cert), Some(key)) = (cert_file, cert_key) {
        let acceptor = make_acceptor_with_cert(&cert, &key);
        match acceptor {
            Ok(acc) => {
                debug!("Created TLS acceptor");
                return Some(acc);
            }
            Err(err) => {
                error!("Could not create TLS acceptor: {}", err);
            }
        }
    }
    debug!("Running websocket server without TLS");
    None
}

pub fn start_server(parameters: ServerParameters, shared_data: SharedData) {
    let address = parameters.address.to_string();
    let port = parameters.port;
    debug!("Start websocket server on {}:{}", address, parameters.port);
    #[cfg(feature = "secure-websocket")]
    let acceptor = make_acceptor(&parameters.cert_file, &parameters.cert_pass);

    thread::spawn(move || {
        let ws_result = TcpListener::bind(format!("{}:{}", address, port));
        if let Ok(server) = ws_result {
            for stream in server.incoming() {
                let shared_data_inst = shared_data.clone();
                let now = Instant::now();
                let local_data = LocalData {
                    last_cap_peak_time: now,
                    last_cap_rms_time: now,
                    last_pb_peak_time: now,
                    last_pb_rms_time: now,
                };
                #[cfg(feature = "secure-websocket")]
                let acceptor_inst = acceptor.clone();

                #[cfg(feature = "secure-websocket")]
                thread::spawn(move || match acceptor_inst {
                    None => {
                        let websocket_res = accept_plain_stream(stream);
                        handle_tcp(websocket_res, &shared_data_inst, local_data);
                    }
                    Some(acc) => {
                        let websocket_res = accept_secure_stream(acc, stream);
                        handle_tls(websocket_res, &shared_data_inst, local_data);
                    }
                });
                #[cfg(not(feature = "secure-websocket"))]
                thread::spawn(move || {
                    let websocket_res = accept_plain_stream(stream);
                    handle_tcp(websocket_res, &shared_data_inst, local_data);
                });
            }
        } else if let Err(err) = ws_result {
            error!("Failed to start websocket server: {}", err);
        }
    });
}

macro_rules! make_handler {
    ($t:ty, $n:ident) => {
        fn $n(
            websocket_res: Res<WebSocket<$t>>,
            shared_data_inst: &SharedData,
            mut local_data: LocalData,
        ) {
            match websocket_res {
                Ok(mut websocket) => loop {
                    let msg_res = websocket.read_message();
                    match msg_res {
                        Ok(msg) => {
                            trace!("received: {:?}", msg);
                            let command = parse_command(msg);
                            debug!("parsed command: {:?}", command);
                            let reply = match command {
                                Ok(cmd) => handle_command(cmd, &shared_data_inst, &mut local_data),
                                Err(err) => Some(WsReply::Invalid {
                                    error: err.to_string(),
                                }),
                            };
                            if let Some(rep) = reply {
                                let write_result = websocket.write_message(Message::text(
                                    serde_json::to_string(&rep).unwrap(),
                                ));
                                if let Err(err) = write_result {
                                    warn!("Failed to write: {}", err);
                                    break;
                                }
                            } else {
                                debug!("Sending no reply");
                            }
                        }
                        Err(tungstenite::error::Error::ConnectionClosed) => {
                            debug!("Connection was closed");
                            break;
                        }
                        Err(err) => {
                            warn!("Lost connection: {}", err);
                            break;
                        }
                    }
                },
                Err(err) => warn!("Connection failed: {}", err),
            };
        }
    };
}

make_handler!(TcpStream, handle_tcp);
#[cfg(feature = "secure-websocket")]
make_handler!(TlsStream<TcpStream>, handle_tls);

#[cfg(feature = "secure-websocket")]
fn accept_secure_stream(
    acceptor: Arc<TlsAcceptor>,
    stream: Result<TcpStream, std::io::Error>,
) -> Res<tungstenite::WebSocket<TlsStream<TcpStream>>> {
    let ws = accept(acceptor.accept(stream?)?)?;
    Ok(ws)
}

fn accept_plain_stream(
    stream: Result<TcpStream, std::io::Error>,
) -> Res<tungstenite::WebSocket<TcpStream>> {
    let ws = accept(stream?)?;
    Ok(ws)
}

fn handle_command(
    command: WsCommand,
    shared_data_inst: &SharedData,
    local_data: &mut LocalData,
) -> Option<WsReply> {
    match command {
        WsCommand::GetCaptureRate => {
            let capstat = shared_data_inst.capture_status.read().unwrap();
            Some(WsReply::GetCaptureRate {
                result: WsResult::Ok,
                value: capstat.measured_samplerate,
            })
        }
        WsCommand::GetSignalRange => {
            let capstat = shared_data_inst.capture_status.read().unwrap();
            Some(WsReply::GetSignalRange {
                result: WsResult::Ok,
                value: capstat.signal_range,
            })
        }
        WsCommand::GetCaptureSignalRms => {
            let values = get_capture_signal_rms(shared_data_inst);
            Some(WsReply::GetCaptureSignalRms {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetCaptureSignalRmsSince(secs) => {
            let values = get_capture_signal_rms_since(shared_data_inst, secs);
            Some(WsReply::GetCaptureSignalRmsSince {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetCaptureSignalRmsSinceLast => {
            let values = get_capture_signal_rms_since_last(shared_data_inst, local_data);
            Some(WsReply::GetCaptureSignalRmsSinceLast {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalRms => {
            let values = get_playback_signal_rms(shared_data_inst);
            Some(WsReply::GetPlaybackSignalRms {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalRmsSince(secs) => {
            let values = get_playback_signal_rms_since(shared_data_inst, secs);
            Some(WsReply::GetPlaybackSignalRmsSince {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalRmsSinceLast => {
            let values = get_playback_signal_rms_since_last(shared_data_inst, local_data);
            Some(WsReply::GetPlaybackSignalRmsSinceLast {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetCaptureSignalPeak => {
            let values = get_capture_signal_peak(shared_data_inst);
            Some(WsReply::GetCaptureSignalPeak {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetCaptureSignalPeakSince(secs) => {
            let values = get_capture_signal_peak_since(shared_data_inst, secs);
            Some(WsReply::GetCaptureSignalPeakSince {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetCaptureSignalPeakSinceLast => {
            let values = get_capture_signal_peak_since_last(shared_data_inst, local_data);
            Some(WsReply::GetCaptureSignalPeakSinceLast {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalPeak => {
            let values = get_playback_signal_peak(shared_data_inst);
            Some(WsReply::GetPlaybackSignalPeak {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalPeakSince(secs) => {
            let values = get_playback_signal_peak_since(shared_data_inst, secs);
            Some(WsReply::GetPlaybackSignalPeakSince {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetPlaybackSignalPeakSinceLast => {
            let values = get_playback_signal_peak_since_last(shared_data_inst, local_data);
            Some(WsReply::GetPlaybackSignalPeakSinceLast {
                result: WsResult::Ok,
                value: values,
            })
        }
        WsCommand::GetSignalLevels => {
            let levels = AllLevels {
                playback_rms: get_playback_signal_rms(shared_data_inst),
                playback_peak: get_playback_signal_peak(shared_data_inst),
                capture_rms: get_capture_signal_rms(shared_data_inst),
                capture_peak: get_capture_signal_peak(shared_data_inst),
            };
            let result = WsReply::GetSignalLevels {
                result: WsResult::Ok,
                value: levels,
            };
            Some(result)
        }
        WsCommand::GetSignalLevelsSince(secs) => {
            let levels = AllLevels {
                playback_rms: get_playback_signal_rms_since(shared_data_inst, secs),
                playback_peak: get_playback_signal_peak_since(shared_data_inst, secs),
                capture_rms: get_capture_signal_rms_since(shared_data_inst, secs),
                capture_peak: get_capture_signal_peak_since(shared_data_inst, secs),
            };
            let result = WsReply::GetSignalLevelsSince {
                result: WsResult::Ok,
                value: levels,
            };
            Some(result)
        }
        WsCommand::GetSignalLevelsSinceLast => {
            let levels = AllLevels {
                playback_rms: get_playback_signal_rms_since_last(shared_data_inst, local_data),
                playback_peak: get_playback_signal_peak_since_last(shared_data_inst, local_data),
                capture_rms: get_capture_signal_rms_since_last(shared_data_inst, local_data),
                capture_peak: get_capture_signal_peak_since_last(shared_data_inst, local_data),
            };
            let result = WsReply::GetSignalLevelsSinceLast {
                result: WsResult::Ok,
                value: levels,
            };
            Some(result)
        }
        WsCommand::GetSignalPeaksSinceStart => {
            let levels = PbCapLevels {
                playback: get_playback_signal_global_peak(shared_data_inst),
                capture: get_capture_signal_global_peak(shared_data_inst),
            };
            let result = WsReply::GetSignalPeaksSinceStart {
                result: WsResult::Ok,
                value: levels,
            };
            Some(result)
        }
        WsCommand::ResetSignalPeaksSinceStart => {
            reset_playback_signal_global_peak(shared_data_inst);
            reset_capture_signal_global_peak(shared_data_inst);
            let result = WsReply::ResetSignalPeaksSinceStart {
                result: WsResult::Ok,
            };
            Some(result)
        }
        WsCommand::GetVersion => Some(WsReply::GetVersion {
            result: WsResult::Ok,
            value: crate_version!().to_string(),
        }),
        WsCommand::GetState => {
            let capstat = shared_data_inst.capture_status.read().unwrap();
            Some(WsReply::GetState {
                result: WsResult::Ok,
                value: capstat.state,
            })
        }
        WsCommand::GetStopReason => {
            let stat = shared_data_inst.status.read().unwrap();
            let value = stat.stop_reason.clone();
            Some(WsReply::GetStopReason {
                result: WsResult::Ok,
                value,
            })
        }
        WsCommand::GetRateAdjust => {
            let capstat = shared_data_inst.capture_status.read().unwrap();
            Some(WsReply::GetRateAdjust {
                result: WsResult::Ok,
                value: capstat.rate_adjust,
            })
        }
        WsCommand::GetClippedSamples => {
            let pbstat = shared_data_inst.playback_status.read().unwrap();
            Some(WsReply::GetClippedSamples {
                result: WsResult::Ok,
                value: pbstat.clipped_samples,
            })
        }
        WsCommand::GetBufferLevel => {
            let pbstat = shared_data_inst.playback_status.read().unwrap();
            Some(WsReply::GetBufferLevel {
                result: WsResult::Ok,
                value: pbstat.buffer_level,
            })
        }
        WsCommand::GetUpdateInterval => {
            let capstat = shared_data_inst.capture_status.read().unwrap();
            Some(WsReply::GetUpdateInterval {
                result: WsResult::Ok,
                value: capstat.update_interval,
            })
        }
        WsCommand::SetUpdateInterval(nbr) => {
            shared_data_inst
                .capture_status
                .write()
                .unwrap()
                .update_interval = nbr;
            shared_data_inst
                .playback_status
                .write()
                .unwrap()
                .update_interval = nbr;
            Some(WsReply::SetUpdateInterval {
                result: WsResult::Ok,
            })
        }
        WsCommand::GetVolume => {
            let procstat = shared_data_inst.processing_status.read().unwrap();
            Some(WsReply::GetVolume {
                result: WsResult::Ok,
                value: procstat.volume,
            })
        }
        WsCommand::SetVolume(nbr) => {
            let mut procstat = shared_data_inst.processing_status.write().unwrap();
            procstat.volume = nbr;
            // Clamp to -150 .. 50 dB, probably larger than needed..
            if procstat.volume < -150.0 {
                procstat.volume = -150.0;
                warn!("Clamped volume at -150 dB")
            } else if procstat.volume > 50.0 {
                procstat.volume = 50.0;
                warn!("Clamped volume at +50 dB")
            }
            Some(WsReply::SetVolume {
                result: WsResult::Ok,
            })
        }
        WsCommand::AdjustVolume(nbr) => {
            let mut procstat = shared_data_inst.processing_status.write().unwrap();
            procstat.volume += nbr;
            // Clamp to -150 .. 50 dB, probably larger than needed..
            if procstat.volume < -150.0 {
                procstat.volume = -150.0;
                warn!("Clamped volume at -150 dB")
            } else if procstat.volume > 50.0 {
                procstat.volume = 50.0;
                warn!("Clamped volume at +50 dB")
            }
            Some(WsReply::AdjustVolume {
                result: WsResult::Ok,
                value: procstat.volume,
            })
        }
        WsCommand::GetMute => {
            let procstat = shared_data_inst.processing_status.read().unwrap();
            Some(WsReply::GetMute {
                result: WsResult::Ok,
                value: procstat.mute,
            })
        }
        WsCommand::SetMute(mute) => {
            let mut procstat = shared_data_inst.processing_status.write().unwrap();
            procstat.mute = mute;
            Some(WsReply::SetMute {
                result: WsResult::Ok,
            })
        }
        WsCommand::ToggleMute => {
            let mut procstat = shared_data_inst.processing_status.write().unwrap();
            procstat.mute = !procstat.mute;
            Some(WsReply::ToggleMute {
                result: WsResult::Ok,
                value: procstat.mute,
            })
        }
        WsCommand::SetConfigJson(config_json) => {
            match serde_json::from_str::<config::Configuration>(&config_json) {
                Ok(mut conf) => match config::validate_config(&mut conf, None) {
                    Ok(()) => {
                        shared_data_inst.command_sender.send(ControllerMessage::ConfigChanged(conf)).unwrap();
                        Some(WsReply::SetConfigJson {
                            result: WsResult::Ok,
                        })
                    }
                    Err(error) => {
                        error!("Error setting config: {}", error);
                        Some(WsReply::SetConfigJson {
                            result: WsResult::Error,
                        })
                    }
                },
                Err(error) => {
                    error!("Config error: {}", error);
                    Some(WsReply::SetConfigJson {
                        result: WsResult::Error,
                    })
                }
            }
        }
        WsCommand::ValidateConfig(config_yml) => {
            match serde_yaml::from_str::<config::Configuration>(&config_yml) {
                Ok(mut conf) => match config::validate_config(&mut conf, None) {
                    Ok(()) => Some(WsReply::ValidateConfig {
                        result: WsResult::Ok,
                        value: serde_yaml::to_string(&conf).unwrap(),
                    }),
                    Err(error) => {
                        error!("Config error: {}", error);
                        Some(WsReply::ValidateConfig {
                            result: WsResult::Error,
                            value: error.to_string(),
                        })
                    }
                },
                Err(error) => {
                    error!("Config error: {}", error);
                    Some(WsReply::ValidateConfig {
                        result: WsResult::Error,
                        value: error.to_string(),
                    })
                }
            }
        }
        WsCommand::Stop => {
            shared_data_inst.command_sender.send(ControllerMessage::Stop).unwrap();
            Some(WsReply::Stop {
                result: WsResult::Ok,
            })
        }
        WsCommand::Exit => {
            shared_data_inst.command_sender.send(ControllerMessage::Exit).unwrap();
            Some(WsReply::Exit {
                result: WsResult::Ok,
            })
        }
        WsCommand::GetSupportedDeviceTypes => {
            let devs = list_supported_devices();
            Some(WsReply::GetSupportedDeviceTypes {
                result: WsResult::Ok,
                value: devs,
            })
        }
        WsCommand::None => None,
    }
}

fn get_playback_signal_peak_since(shared_data: &SharedData, time: f32) -> Vec<f32> {
    let time_instant = Instant::now() - Duration::from_secs_f32(time);
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_peak
        .get_max_since(time_instant);
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_playback_signal_rms_since(shared_data: &SharedData, time: f32) -> Vec<f32> {
    let time_instant = Instant::now() - Duration::from_secs_f32(time);
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_rms
        .get_average_sqrt_since(time_instant);
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_peak_since(shared_data: &SharedData, time: f32) -> Vec<f32> {
    let time_instant = Instant::now() - Duration::from_secs_f32(time);
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_peak
        .get_max_since(time_instant);
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_rms_since(shared_data: &SharedData, time: f32) -> Vec<f32> {
    let time_instant = Instant::now() - Duration::from_secs_f32(time);
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_rms
        .get_average_sqrt_since(time_instant);
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_playback_signal_peak_since_last(
    shared_data: &SharedData,
    local_data: &mut LocalData,
) -> Vec<f32> {
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_peak
        .get_max_since(local_data.last_pb_peak_time);
    match res {
        Some(mut record) => {
            local_data.last_pb_peak_time = record.time;
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_playback_signal_rms_since_last(
    shared_data: &SharedData,
    local_data: &mut LocalData,
) -> Vec<f32> {
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_rms
        .get_average_sqrt_since(local_data.last_pb_rms_time);
    match res {
        Some(mut record) => {
            local_data.last_pb_rms_time = record.time;
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_peak_since_last(
    shared_data: &SharedData,
    local_data: &mut LocalData,
) -> Vec<f32> {
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_peak
        .get_max_since(local_data.last_cap_peak_time);
    match res {
        Some(mut record) => {
            local_data.last_cap_peak_time = record.time;
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_rms_since_last(
    shared_data: &SharedData,
    local_data: &mut LocalData,
) -> Vec<f32> {
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_rms
        .get_average_sqrt_since(local_data.last_cap_rms_time);
    match res {
        Some(mut record) => {
            local_data.last_cap_rms_time = record.time;
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_playback_signal_peak(shared_data: &SharedData) -> Vec<f32> {
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_peak
        .get_last();
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_playback_signal_global_peak(shared_data: &SharedData) -> Vec<f32> {
    shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_peak
        .get_global_max()
}

fn reset_playback_signal_global_peak(shared_data: &SharedData) {
    shared_data
        .playback_status
        .write()
        .unwrap()
        .signal_peak
        .reset_global_max();
}

fn get_playback_signal_rms(shared_data: &SharedData) -> Vec<f32> {
    let res = shared_data
        .playback_status
        .read()
        .unwrap()
        .signal_rms
        .get_last_sqrt();
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_peak(shared_data: &SharedData) -> Vec<f32> {
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_peak
        .get_last();
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}

fn get_capture_signal_global_peak(shared_data: &SharedData) -> Vec<f32> {
    shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_peak
        .get_global_max()
}

fn reset_capture_signal_global_peak(shared_data: &SharedData) {
    shared_data
        .capture_status
        .write()
        .unwrap()
        .signal_peak
        .reset_global_max();
}

fn get_capture_signal_rms(shared_data: &SharedData) -> Vec<f32> {
    let res = shared_data
        .capture_status
        .read()
        .unwrap()
        .signal_rms
        .get_last_sqrt();
    match res {
        Some(mut record) => {
            linear_to_db(&mut record.values);
            record.values
        }
        None => vec![],
    }
}