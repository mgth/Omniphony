pub mod burst;
pub mod iamf;
pub mod parser;

pub use parser::{
    Iec61937Packet, SYNC_BYTES, SYNCWORD_PA, SYNCWORD_PB, SpdifParser, contains_sync, find_sync,
    is_pause,
};

pub use burst::{BurstError, DATA_TYPE_IAMF_PRIVATE, write_iamf_burst};
