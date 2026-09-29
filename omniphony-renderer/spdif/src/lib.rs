pub mod parser;

pub use parser::{
    Iec61937Packet, SYNC_BYTES, SYNCWORD_PA, SYNCWORD_PB, SpdifParser, contains_sync, find_sync,
};
