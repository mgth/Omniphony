use log::LevelFilter;
use rosc::{OscMessage, OscType};

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    SaveConfig,
    ReloadConfig,
    Quit,
    SetLogLevel(LevelFilter),
}

pub fn parse_runtime_log_level(value: &str) -> Option<LevelFilter> {
    match value.trim().to_ascii_lowercase().as_str() {
        "off" => Some(LevelFilter::Off),
        "error" => Some(LevelFilter::Error),
        "warn" | "warning" => Some(LevelFilter::Warn),
        "info" => Some(LevelFilter::Info),
        "debug" => Some(LevelFilter::Debug),
        "trace" => Some(LevelFilter::Trace),
        _ => None,
    }
}

pub fn parse_process_command(msg: &OscMessage) -> Option<RuntimeCommand> {
    match msg.addr.as_str() {
        "/omniphony/control/save_config" => Some(RuntimeCommand::SaveConfig),
        "/omniphony/control/reload_config" => Some(RuntimeCommand::ReloadConfig),
        "/omniphony/control/quit" => Some(RuntimeCommand::Quit),
        "/omniphony/control/log_level" => msg.args.first().and_then(|arg| match arg {
            OscType::String(s) => parse_runtime_log_level(s).map(RuntimeCommand::SetLogLevel),
            _ => None,
        }),
        _ => None,
    }
}
