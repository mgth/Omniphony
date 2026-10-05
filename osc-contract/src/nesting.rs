//! How deep a datagram may nest, and the check that reads it off the bytes.
//!
//! Both ends of the wire decode what they receive with `rosc`, which decodes
//! a bundle by recursion, one level of its stack per level of nesting, and
//! frees the value it builds for nested arrays by recursion too. Twenty bytes
//! of datagram buy a level of bundle and two a level of array, so a
//! 65,536-byte receive buffer holds thousands of either: enough to overflow
//! the listener thread's stack, which aborts the process. One datagram from
//! anyone the listener accepts does it.
//!
//! So the contract bounds nesting at [`MAX_NESTING`], and a listener runs
//! [`check`] on the raw bytes before it hands a datagram to its decoder. The
//! check does not recurse, allocates nothing and needs no OSC library, which
//! is what lets it live in the crate both ends share: one walk and one limit
//! instead of a copy on each side.
//!
//! The tests here hold the walk to the bytes. The ones that hold it to what
//! `rosc` actually decodes are in the engine (`orender_engine::osc::decode`),
//! where the decoder is.

/// How deep bundles may nest, and arrays within one message's arguments. The
/// protocol's own bundles are one level deep and it sends no array, so a
/// client keeps plenty of room for framing of its own, and a decoder never
/// recurses more than a few levels.
pub const MAX_NESTING: usize = 8;

const BUNDLE_TAG: &[u8] = b"#bundle\0";
const TIME_TAG_LEN: usize = 8;

/// Refuse a datagram that nests deeper than [`MAX_NESTING`], with the reason.
/// To be called on every datagram received from the network, before decoding.
///
/// Walks the framing the decoder recurses on, with a fixed stack.
///
/// A packet that starts with the bundle tag holds a time tag, then elements:
/// a 4-byte size and that many bytes, a packet in turn. Anything else is a
/// message, where only the type tags can nest. Where the framing breaks, the
/// decoder gives up on the bundle; this goes on to what follows instead, so
/// it visits everything the decoder does and never reads a datagram as
/// shallower than the decoder finds it.
pub fn check(datagram: &[u8]) -> Result<(), &'static str> {
    // Where each bundle being walked ends, outermost first.
    let mut bundle_ends = [0usize; MAX_NESTING];
    let mut depth = 0;
    // The packet looked at: `datagram[start..end]`.
    let (mut start, mut end) = (0, datagram.len());
    loop {
        if datagram[start..end].starts_with(BUNDLE_TAG) {
            if depth == MAX_NESTING {
                return Err("bundles nested too deep");
            }
            bundle_ends[depth] = end;
            depth += 1;
            // The tag is padded to a 4-byte boundary of the datagram.
            start = (start + BUNDLE_TAG.len()).next_multiple_of(4) + TIME_TAG_LEN;
        } else {
            if array_depth(datagram, start, end) > MAX_NESTING {
                return Err("arrays nested too deep");
            }
            start = end;
        }
        // On to the next element, of the innermost bundle that has one left.
        loop {
            let Some(&bundle_end) = depth.checked_sub(1).map(|outer| &bundle_ends[outer]) else {
                return Ok(());
            };
            if let Some(element) = element_at(datagram, start, bundle_end) {
                (start, end) = element;
                break;
            }
            start = bundle_end;
            depth -= 1;
        }
    }
}

/// The packet of the element at `at` in a bundle that ends at `bundle_end`.
/// `None` at the end of the bundle, and where the size runs past it.
fn element_at(datagram: &[u8], at: usize, bundle_end: usize) -> Option<(usize, usize)> {
    let start = at.checked_add(4).filter(|&start| start <= bundle_end)?;
    let size = u32::from_be_bytes(datagram[at..start].try_into().ok()?) as usize;
    let end = start.checked_add(size).filter(|&end| end <= bundle_end)?;
    Some((start, end))
}

/// How deep the arrays of the message `datagram[start..end]` nest, read from
/// its type tags: the string after the padded address, where `[` opens an
/// array and `]` closes one. An array left open counts, though the decoder
/// builds nothing nested from it.
fn array_depth(datagram: &[u8], start: usize, end: usize) -> usize {
    let Some(address_len) = datagram[start..end].iter().position(|&byte| byte == 0) else {
        return 0;
    };
    let tags_start = (start + address_len + 1).next_multiple_of(4);
    let Some(tags) = datagram.get(tags_start..end) else {
        return 0;
    };
    let (mut depth, mut deepest) = (0usize, 0usize);
    for &tag in tags.iter().take_while(|&&tag| tag != 0) {
        match tag {
            b'[' => {
                depth += 1;
                deepest = deepest.max(depth);
            }
            b']' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    deepest
}

/// `depth` bundles nested one inside the other around a single message with
/// no argument: 20 bytes a level, plus 8.
///
/// What the tests of a listener send it, on both ends; nothing else has a use
/// for it.
pub fn nested_bundles(depth: usize) -> Vec<u8> {
    let mut datagram = Vec::with_capacity(depth * 20 + 8);
    for inner in (0..depth).rev() {
        datagram.extend_from_slice(BUNDLE_TAG);
        datagram.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        datagram.extend_from_slice(&((inner * 20 + 8) as u32).to_be_bytes());
    }
    datagram.extend_from_slice(b"/a\0\0,\0\0\0");
    datagram
}

/// A message whose only argument is `depth` empty arrays nested one inside
/// the other: 2 bytes a level. For the same tests as [`nested_bundles`].
pub fn nested_arrays(depth: usize) -> Vec<u8> {
    let mut datagram = b"/a\0\0,".to_vec();
    datagram.extend(std::iter::repeat_n(b'[', depth));
    datagram.extend(std::iter::repeat_n(b']', depth));
    datagram.push(0);
    datagram.resize(datagram.len().next_multiple_of(4), 0);
    datagram
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An OSC string: the bytes, a NUL, padded to four.
    fn string(bytes: &[u8]) -> Vec<u8> {
        let mut out = bytes.to_vec();
        out.push(0);
        out.resize(out.len().next_multiple_of(4), 0);
        out
    }

    /// A message to `/a` with these type tags and these argument bytes.
    fn message(tags: &str, arguments: &[u8]) -> Vec<u8> {
        let mut out = string(b"/a");
        out.extend(string(format!(",{tags}").as_bytes()));
        out.extend_from_slice(arguments);
        out
    }

    /// A bundle of these packets.
    fn bundle(elements: &[Vec<u8>]) -> Vec<u8> {
        let mut out = BUNDLE_TAG.to_vec();
        out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        for element in elements {
            out.extend_from_slice(&(element.len() as u32).to_be_bytes());
            out.extend_from_slice(element);
        }
        out
    }

    /// The datagrams the listeners' tests send are what they are said to be.
    #[test]
    fn the_nested_datagrams_are_built_as_described() {
        let innermost = message("", &[]);
        assert_eq!(nested_bundles(0), innermost);
        assert_eq!(nested_bundles(2), bundle(&[bundle(&[innermost])]));
        assert_eq!(nested_arrays(0), message("", &[]));
        assert_eq!(nested_arrays(3), message("[[[]]]", &[]));
        // The sizes the listeners' tests count on.
        assert_eq!(nested_bundles(3_000).len(), 60_008);
        assert_eq!(nested_arrays(30_000).len(), 60_008);
    }

    #[test]
    fn bundles_pass_up_to_the_limit_and_no_deeper() {
        assert_eq!(check(&nested_bundles(MAX_NESTING)), Ok(()));
        assert_eq!(
            check(&nested_bundles(MAX_NESTING + 1)),
            Err("bundles nested too deep")
        );
        // As many levels as a listener's buffer holds.
        assert!(check(&nested_bundles(3_276)).is_err());
    }

    #[test]
    fn arrays_pass_up_to_the_limit_and_no_deeper() {
        assert_eq!(check(&nested_arrays(MAX_NESTING)), Ok(()));
        assert_eq!(
            check(&nested_arrays(MAX_NESTING + 1)),
            Err("arrays nested too deep")
        );
        assert!(check(&nested_arrays(32_000)).is_err());
    }

    /// Every message of a bundle is looked at, wherever it sits, and the two
    /// limits are counted apart: a message at the deepest level of bundle may
    /// still nest its arrays to the limit.
    #[test]
    fn arrays_are_counted_in_every_message_of_a_bundle() {
        let flat = message("if", &[0; 8]);
        let deep = nested_arrays(MAX_NESTING + 1);
        for elements in [
            vec![deep.clone(), flat.clone()],
            vec![flat.clone(), deep.clone()],
            vec![flat.clone(), bundle(&[flat.clone(), deep.clone()])],
        ] {
            assert_eq!(check(&bundle(&elements)), Err("arrays nested too deep"));
        }

        let mut both_at_the_limit = nested_arrays(MAX_NESTING);
        for _ in 0..MAX_NESTING {
            both_at_the_limit = bundle(&[flat.clone(), both_at_the_limit]);
        }
        assert_eq!(check(&both_at_the_limit), Ok(()));
    }

    /// Depth is nesting, not count: a state snapshot is one bundle of many
    /// messages, and a client may send as many bundles side by side. Nor do
    /// arrays that follow one another add up.
    #[test]
    fn what_sits_side_by_side_is_not_deep() {
        let siblings: Vec<_> = (0..500).map(|_| bundle(&[message("", &[])])).collect();
        assert_eq!(check(&bundle(&siblings)), Ok(()));
        assert_eq!(check(&message(&"[]".repeat(500), &[])), Ok(()));
    }

    /// Only the framing counts. A layout travels as JSON in a string, full of
    /// brackets, and a blob may hold any bytes, a bundle's included.
    #[test]
    fn brackets_and_bundle_tags_inside_arguments_are_not_nesting() {
        let json = "[".repeat(100) + &"]".repeat(100);
        let blob = nested_bundles(100);
        let mut arguments = string(json.as_bytes());
        arguments.extend_from_slice(&(blob.len() as u32).to_be_bytes());
        arguments.extend_from_slice(&blob);
        assert_eq!(check(&message("sb", &arguments)), Ok(()));
    }

    /// Whatever arrives is walked to the end without a panic: datagrams at
    /// every depth around the limit, cut short at every length, then with
    /// bytes overwritten, sizes included.
    #[test]
    fn no_datagram_makes_the_walk_panic() {
        fn next(seed: &mut u64) -> u64 {
            *seed ^= *seed << 13;
            *seed ^= *seed >> 7;
            *seed ^= *seed << 17;
            *seed
        }

        let mut seed = 0x9e37_79b9_7f4a_7c15;
        for depth in 0..MAX_NESTING + 4 {
            let mut datagram = message("f[s]", &[0; 12]);
            for _ in 0..depth {
                datagram = bundle(&[nested_arrays(depth), datagram]);
            }
            for len in 0..datagram.len() {
                let _ = check(&datagram[..len]);
            }
            for _ in 0..2_000 {
                let mut corrupted = datagram.clone();
                for _ in 0..=next(&mut seed) % 3 {
                    let at = (next(&mut seed) % corrupted.len() as u64) as usize;
                    corrupted[at] = next(&mut seed) as u8;
                }
                let _ = check(&corrupted);
            }
        }
        for hostile in [
            &[][..],
            b"#bundle\0",
            b"#bundle\0\0\0\0\0\0\0\0\x01\xff\xff\xff\xff",
        ] {
            assert_eq!(check(hostile), Ok(()));
        }
    }
}
