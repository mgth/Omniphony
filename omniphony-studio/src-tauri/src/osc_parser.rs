//! Both Studio frontends use the same wire parser and event types.
//! Protocol additions and regression tests belong in the toolkit-free core.
#[allow(unused_imports)] // Preserve the existing module API for host consumers.
pub use omniphony_studio_core::osc::parser::{
    is_heartbeat_address, parse_osc_message, CoordinateFormat, HeartbeatResponse, LogEntry,
    OscEvent, Position,
};
