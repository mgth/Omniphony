use crate::osc_contract;
use log::LevelFilter;
use rosc::{OscMessage, OscType};

#[derive(Debug, Clone)]
pub enum RuntimeCommand {
    SaveConfig,
    ReloadConfig,
    Quit,
    /// Another local instance wants the OSC RX port; only honoured by
    /// instances started with `--osc-yield` (see `sys::shutdown::is_yieldable`).
    YieldPort,
    /// Sent to a standing-by instance (on its advertised dynamic resume port) to
    /// re-acquire the OSC port + audio and resume rendering.
    Resume,
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
        osc_contract::CONTROL_SAVE_CONFIG => Some(RuntimeCommand::SaveConfig),
        osc_contract::CONTROL_RELOAD_CONFIG => Some(RuntimeCommand::ReloadConfig),
        osc_contract::CONTROL_QUIT => Some(RuntimeCommand::Quit),
        osc_contract::CONTROL_YIELD_PORT => Some(RuntimeCommand::YieldPort),
        osc_contract::CONTROL_RESUME => Some(RuntimeCommand::Resume),
        osc_contract::CONTROL_LOG_LEVEL => msg.args.first().and_then(|arg| match arg {
            OscType::String(s) => parse_runtime_log_level(s).map(RuntimeCommand::SetLogLevel),
            _ => None,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(addr: &str) -> OscMessage {
        OscMessage {
            addr: addr.to_string(),
            args: vec![],
        }
    }

    #[test]
    fn yield_port_maps_to_runtime_command() {
        assert!(matches!(
            parse_process_command(&msg(osc_contract::CONTROL_YIELD_PORT)),
            Some(RuntimeCommand::YieldPort)
        ));
        assert!(matches!(
            parse_process_command(&msg(osc_contract::CONTROL_QUIT)),
            Some(RuntimeCommand::Quit)
        ));
        assert!(parse_process_command(&msg(osc_contract::CONTROL_UNKNOWN)).is_none());
    }
}
