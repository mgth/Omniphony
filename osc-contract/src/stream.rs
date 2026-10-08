//! OSC 1.0 stream framing, as the engine's stream transport carries it
//! (revision 2, `docs/control-transport.md`): each packet preceded by its
//! size, a big-endian int32. One implementation for both ends, the engine and
//! Studio. `std::io` only, like the rest of this crate.

use std::io::{self, Read};

/// Largest packet either end sends on a stream: a whole state snapshot
/// travels as one.
pub const MAX_PACKET: usize = 1 << 20;

/// `packet`, framed: its size, then its bytes.
pub fn frame(packet: &[u8]) -> io::Result<Vec<u8>> {
    let size = u32::try_from(packet.len())
        .ok()
        .filter(|&size| size as usize <= MAX_PACKET)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a {}-byte packet, over the {MAX_PACKET}-byte limit",
                    packet.len()
                ),
            )
        })?;
    let mut framed = Vec::with_capacity(packet.len() + 4);
    framed.extend_from_slice(&size.to_be_bytes());
    framed.extend_from_slice(packet);
    Ok(framed)
}

/// Read one framed packet: `Ok(None)` at a clean end of stream (between
/// packets), an error for a size over `max` or a connection that failed.
pub fn read_frame(input: &mut impl Read, max: usize) -> io::Result<Option<Vec<u8>>> {
    let mut size = [0u8; 4];
    match input.read_exact(&mut size) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let size = u32::from_be_bytes(size) as usize;
    if size > max {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a {size}-byte packet, over the {max}-byte limit"),
        ));
    }
    let mut packet = vec![0u8; size];
    input.read_exact(&mut packet)?;
    Ok(Some(packet))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_and_end_cleanly() {
        let mut wire = Vec::new();
        for packet in [&b"one"[..], b"", b"three!!"] {
            wire.extend(frame(packet).unwrap());
        }
        let mut input = &wire[..];
        for want in [&b"one"[..], b"", b"three!!"] {
            assert_eq!(read_frame(&mut input, 64).unwrap().as_deref(), Some(want));
        }
        assert!(read_frame(&mut input, 64).unwrap().is_none());
    }

    #[test]
    fn an_oversized_frame_is_refused_both_ways() {
        let mut bytes = (100u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(&[0; 100]);
        assert!(read_frame(&mut &bytes[..], 64).is_err());
        assert_eq!(
            read_frame(&mut &bytes[..], 100).unwrap().unwrap().len(),
            100
        );
        assert!(frame(&vec![0; MAX_PACKET + 1]).is_err());
        assert!(frame(&vec![0; MAX_PACKET]).is_ok());
    }

    #[test]
    fn a_frame_cut_short_is_an_error_not_an_end() {
        let mut bytes = frame(b"abcdef").unwrap();
        bytes.truncate(7);
        assert!(read_frame(&mut &bytes[..], 64).is_err());
    }
}
