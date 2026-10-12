//! The control listener's decoder: `rosc`'s, behind the contract's bound on
//! nesting.
//!
//! `rosc` decodes a bundle by recursion, one level of its stack per level of
//! nesting, and the value it builds for nested arrays is freed by recursion
//! too. Twenty bytes of datagram buy a level of bundle and two a level of
//! array, so the listener's 65,536-byte buffer holds thousands of either:
//! enough to overflow the listener thread's stack, which aborts the process.
//! One datagram from anyone who can reach the port does it.
//!
//! [`decode_datagram`] has the nesting read off the raw bytes first, without
//! recursing, and refuses the datagram past the contract's limit. The walk is
//! the contract crate's ([`nesting::check`]), where the Studio's listener
//! finds the same one; the tests that hold it to what `rosc` decodes are
//! here, where the decoder is.

use rosc::{OscError, OscPacket};
use runtime_control::osc_contract::nesting;

/// Decode a datagram received from the network: `rosc::decoder::decode_udp`,
/// refusing first what nests deeper than [`nesting::MAX_NESTING`].
pub(crate) fn decode_datagram(datagram: &[u8]) -> Result<(&[u8], OscPacket), OscError> {
    nesting::check(datagram).map_err(OscError::BadPacket)?;
    rosc::decoder::decode_udp(datagram)
}

#[cfg(test)]
mod tests {
    use super::*;
    use nesting::{MAX_NESTING, nested_arrays, nested_bundles};
    use rosc::{OscArray, OscBundle, OscMessage, OscTime, OscType};

    const TIME: OscTime = OscTime {
        seconds: 0,
        fractional: 1,
    };

    fn message(args: Vec<OscType>) -> OscPacket {
        OscPacket::Message(OscMessage {
            addr: "/a".into(),
            args,
        })
    }

    fn bundle(content: Vec<OscPacket>) -> OscPacket {
        OscPacket::Bundle(OscBundle {
            timetag: TIME,
            content,
        })
    }

    fn encoded(packet: &OscPacket) -> Vec<u8> {
        rosc::encoder::encode(packet).unwrap()
    }

    fn refusal(datagram: &[u8]) -> Option<String> {
        decode_datagram(datagram).err().map(|e| e.to_string())
    }

    /// How deep the bundles of a decoded packet nest, and its arrays. Only
    /// ever called on what [`decode_datagram`] let through.
    fn nesting(packet: &OscPacket) -> (usize, usize) {
        fn arrays(arg: &OscType) -> usize {
            match arg {
                OscType::Array(array) => 1 + array.content.iter().map(arrays).max().unwrap_or(0),
                _ => 0,
            }
        }
        match packet {
            OscPacket::Message(message) => (0, message.args.iter().map(arrays).max().unwrap_or(0)),
            OscPacket::Bundle(bundle) => {
                let inner = bundle.content.iter().map(nesting);
                let (bundles, arrays) = inner.fold((0, 0), |a, b| (a.0.max(b.0), a.1.max(b.1)));
                (1 + bundles, arrays)
            }
        }
    }

    #[test]
    fn bundles_decode_up_to_the_limit_and_no_deeper() {
        let at_the_limit = nested_bundles(MAX_NESTING);
        let (_, packet) = decode_datagram(&at_the_limit).expect("at the limit");
        assert_eq!(nesting(&packet), (MAX_NESTING, 0));

        let refused = refusal(&nested_bundles(MAX_NESTING + 1)).expect("past the limit");
        assert!(refused.contains("bundles nested too deep"), "{refused}");
        // As many levels as the listener's buffer holds: refused without the
        // recursion that would not return.
        assert!(refusal(&nested_bundles(3_276)).is_some());
    }

    #[test]
    fn arrays_decode_up_to_the_limit_and_no_deeper() {
        let at_the_limit = nested_arrays(MAX_NESTING);
        let (_, packet) = decode_datagram(&at_the_limit).expect("at the limit");
        assert_eq!(nesting(&packet), (0, MAX_NESTING));

        let refused = refusal(&nested_arrays(MAX_NESTING + 1)).expect("past the limit");
        assert!(refused.contains("arrays nested too deep"), "{refused}");
        assert!(refusal(&nested_arrays(32_000)).is_some());

        // Inside a bundle too, where the message is an element.
        let mut wrapped = nested_bundles(1);
        let deep = nested_arrays(MAX_NESTING + 1);
        wrapped.truncate(16);
        wrapped.extend_from_slice(&(deep.len() as u32).to_be_bytes());
        wrapped.extend_from_slice(&deep);
        assert!(refusal(&wrapped).is_some());
    }

    /// Depth is nesting, not count: a state snapshot is one bundle of many
    /// messages, and a client may send as many bundles side by side.
    #[test]
    fn many_bundles_side_by_side_are_not_deep() {
        let siblings = (0..500).map(|_| bundle(vec![message(vec![])])).collect();
        let datagram = encoded(&bundle(siblings));
        let (_, packet) = decode_datagram(&datagram).expect("two levels");
        assert_eq!(nesting(&packet), (2, 0));
    }

    /// Only the framing counts. A layout travels as JSON in a string, full of
    /// brackets, and a blob may hold any bytes, a bundle's included.
    #[test]
    fn brackets_and_bundle_tags_inside_arguments_are_not_nesting() {
        let json = "[".repeat(100) + &"]".repeat(100);
        let datagram = encoded(&message(vec![
            OscType::String(json.clone()),
            OscType::Blob(nested_bundles(100)),
        ]));
        let (_, packet) = decode_datagram(&datagram).expect("flat arguments");
        assert_eq!(nesting(&packet), (0, 0));
        assert_eq!(
            packet,
            message(vec![json.into(), OscType::Blob(nested_bundles(100))])
        );
    }

    /// The guarantee the listener relies on, against the decoder itself: over
    /// datagrams built at every depth around the limit, then cut short and
    /// corrupted, nothing is let through that decodes to more than
    /// [`MAX_NESTING`] levels, and no input makes the walk panic.
    #[test]
    fn nothing_deeper_than_the_limit_is_ever_let_through() {
        fn tree(bundles: usize, arrays: usize, seed: &mut u64) -> OscPacket {
            let mut array = OscType::Int(next(seed) as i32);
            for _ in 0..arrays {
                array = OscType::Array(OscArray {
                    content: vec![OscType::String("[x]".into()), array],
                });
            }
            let mut packet = message(vec![OscType::Float(1.0), array]);
            for _ in 0..bundles {
                let mut content = vec![message(vec![OscType::Blob(vec![b'['; 3])])];
                content.insert((next(seed) % 2) as usize, packet);
                packet = bundle(content);
            }
            packet
        }
        fn next(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }
        fn check(datagram: &[u8]) {
            if let Ok((_, packet)) = decode_datagram(datagram) {
                let (bundles, arrays) = nesting(&packet);
                assert!(
                    bundles <= MAX_NESTING && arrays <= MAX_NESTING,
                    "{bundles} bundles and {arrays} arrays deep was let through"
                );
            }
        }

        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for bundles in 0..MAX_NESTING + 4 {
            for arrays in 0..MAX_NESTING + 4 {
                let packet = tree(bundles, arrays, &mut seed);
                let datagram = encoded(&packet);
                let within = bundles <= MAX_NESTING && arrays <= MAX_NESTING;
                assert_eq!(
                    decode_datagram(&datagram).is_ok(),
                    within,
                    "{bundles} bundles, {arrays} arrays"
                );
                for len in 0..datagram.len() {
                    check(&datagram[..len]);
                }
                for _ in 0..200 {
                    let mut corrupted = datagram.clone();
                    let at = (next(&mut seed) % corrupted.len() as u64) as usize;
                    corrupted[at] = next(&mut seed) as u8;
                    check(&corrupted);
                }
            }
        }
    }
}
