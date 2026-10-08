//! Several decoder bridges behind one: the host's router
//! (`docs/multi-bridge.md`, "Routing").
//!
//! A [`BridgeSet`] answers the rest of the host the way a single
//! [`FormatBridgeBox`] does — the same methods, called the same way — and
//! picks, for each stream, the bridge that decodes it:
//!
//! - **One bridge**: every call goes straight to it, unprobed. A host with one
//!   bridge behaves exactly as it did before sets existed.
//! - **IEC 61937**: the bridge for a burst type is found by
//!   [`BridgeLib::probe`] once and remembered; a burst type that moves to
//!   another bridge mid-stream resets the old one and reports `did_reset`.
//! - **Raw**: a byte stream, cut anywhere by the reads that carry it. Until a
//!   bridge claims a validated stream start, the undecided bytes are kept
//!   (bounded) and offered to every bridge's probe; the earliest start wins,
//!   load order breaks ties, and the winner receives the bytes from its start
//!   on. The route then holds until [`reset`](BridgeSet::reset).
//! - **After a reset** (a seek), the last route is the fallback: a push whose
//!   first byte starts another bridge's stream moves the route, anything else
//!   goes to the fallback bridge, as the bridges' own detection did before.
//!
//! Nothing here may panic: the methods run on the decode path of a player.

use crate::bridge_loader::{BridgeLibs, install_bridge_host_log_sink};
use abi_stable::std_types::{RSlice, RString, RVec};
use anyhow::{Result, bail};
use bridge_api::{
    BridgeLibRef, FormatBridgeBox, RChannelPose, RChannelTag, RCoordinateFormat, RInputTransport,
    RProbe, RProbeVerdict, RPushResult, RVbapCartesianDefaults, RVbapTableMode,
};

/// A bridge plugin's `probe` entry ([`BridgeLib::probe`]).
pub type ProbeFn = extern "C" fn(RSlice<'_, u8>, RInputTransport, u8) -> RProbe;

/// The most undecided raw bytes kept while no bridge has claimed a stream.
/// Allocated once, when a set of several bridges is opened.
pub const MAX_UNDECIDED_RAW: usize = 64 * 1024;

/// The most new bytes one probe call is shown while undecided, past what it
/// has already seen; a probe that needs more for a candidate says so
/// (`Pending.needed`) and is shown that much. Keeps every byte shown a
/// bounded number of times, however the buffer fills.
const PROBE_WINDOW: usize = 4096;

/// How much of a push is shown first, after a reset, to ask whether it starts
/// a stream: enough for every sync word; a probe that needs more says so
/// (`Pending.needed`), and is shown that much.
const PROBATION_WINDOW: usize = 16;

/// `iec_route` entries that are not a bridge index.
const IEC_UNPROBED: u8 = u8::MAX;
const IEC_NO_BRIDGE: u8 = u8::MAX - 1;

/// One bridge of the set.
struct Slot {
    /// `None` for a set of one opened from a bare instance: never probed.
    probe: Option<ProbeFn>,
    /// Its `input_codecs`, lower case.
    input_codecs: Vec<String>,
    bridge: FormatBridgeBox,
}

/// Where raw input goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RawRoute {
    /// No stream claimed yet: bytes wait in [`Undecided`].
    Undecided,
    /// Every byte goes to this bridge, until `reset`.
    Locked(usize),
    /// After a reset: this bridge takes what no other claims at a push's
    /// first byte.
    Probation(usize),
}

/// The decoder bridges a host holds, routed per stream. See the module doc.
pub struct BridgeSet {
    slots: Vec<Slot>,
    /// The bridge that took the last packet; per-stream answers come from it.
    active: usize,
    /// Whether `active` has taken a packet since the set was opened or
    /// reset. Before that it is only the idle default, so a first burst for
    /// another bridge is no switch: nothing to reset, no new segment.
    fed: bool,
    raw: RawRoute,
    /// The bridge `input_codec` named, which takes raw input unprobed.
    forced: Option<usize>,
    /// Burst type → bridge index, [`IEC_UNPROBED`] or [`IEC_NO_BRIDGE`].
    iec_route: [u8; 256],
    undecided: Undecided,
    /// Bytes no bridge took since the stream started, for the warning.
    unrouted: Unrouted,
}

impl BridgeSet {
    /// One instance of every bridge in `libs`, in load order. Refused when
    /// they disagree on [`coordinate_format`](Self::coordinate_format): the
    /// renderer reads it once, and positions in another format would be
    /// misread.
    pub fn open(libs: &BridgeLibs) -> Result<Self> {
        let slots = libs
            .iter()
            .map(|lib| {
                install_bridge_host_log_sink(lib);
                Self::slot_of(lib)
            })
            .collect();
        Self::from_slots(slots)
    }

    fn slot_of(lib: &BridgeLibRef) -> Slot {
        Slot {
            probe: Some(lib.probe()),
            input_codecs: lib.input_codecs()()
                .iter()
                .map(|codec| codec.as_str().to_ascii_lowercase())
                .collect(),
            bridge: lib.new_bridge()(false),
        }
    }

    /// A set of one around an instance the caller already holds, e.g. a test
    /// double: never probed, every call goes to it.
    pub fn single(bridge: FormatBridgeBox) -> Self {
        Self::from_slots(vec![Slot {
            probe: None,
            input_codecs: Vec::new(),
            bridge,
        }])
        .expect("a single bridge agrees with itself")
    }

    /// A set from bridges given as their probe, their `input_codecs` and an
    /// instance, in load order: how tests build one without loading plugins.
    pub fn from_parts(parts: Vec<(ProbeFn, Vec<String>, FormatBridgeBox)>) -> Result<Self> {
        Self::from_slots(
            parts
                .into_iter()
                .map(|(probe, input_codecs, bridge)| Slot {
                    probe: Some(probe),
                    input_codecs: input_codecs
                        .into_iter()
                        .map(|c| c.to_ascii_lowercase())
                        .collect(),
                    bridge,
                })
                .collect(),
        )
    }

    fn from_slots(slots: Vec<Slot>) -> Result<Self> {
        if slots.is_empty() {
            bail!("no decoder bridge to open");
        }
        if slots.len() > usize::from(IEC_NO_BRIDGE) {
            bail!("too many decoder bridges ({})", slots.len());
        }
        let format = slots[0].bridge.coordinate_format();
        if let Some(other) = slots
            .iter()
            .position(|slot| slot.bridge.coordinate_format() != format)
        {
            bail!(
                "the decoder bridges disagree on their coordinate format \
                 (bridge 1: {format:?}, bridge {}: {:?}); load bridges of one format",
                other + 1,
                slots[other].bridge.coordinate_format()
            );
        }
        let undecided = if slots.len() > 1 {
            Undecided::new(slots.len())
        } else {
            Undecided::default()
        };
        Ok(Self {
            slots,
            active: 0,
            fed: false,
            raw: RawRoute::Undecided,
            forced: None,
            iec_route: [IEC_UNPROBED; 256],
            undecided,
            unrouted: Unrouted::default(),
        })
    }

    /// How many bridges the set holds.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Never true: a set holds at least one bridge.
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Push one input unit to the bridge whose stream it belongs to (see the
    /// module doc), and return what that bridge decoded.
    pub fn push_packet(
        &mut self,
        data: &[u8],
        transport: RInputTransport,
        data_type: u8,
    ) -> RPushResult {
        if self.slots.len() == 1 {
            return self.slots[0]
                .bridge
                .push_packet(data.into(), transport, data_type);
        }
        match transport {
            RInputTransport::Iec61937 => self.push_iec61937(data, data_type),
            RInputTransport::Raw => self.push_raw(data),
        }
    }

    fn push_iec61937(&mut self, data: &[u8], data_type: u8) -> RPushResult {
        let Some(index) = self.iec_bridge(data, data_type) else {
            self.unrouted.note(data.len(), || {
                format!("no decoder bridge takes IEC 61937 burst type {data_type:#04x}")
            });
            return empty_result();
        };
        let switched = index != self.active && self.fed;
        if switched {
            self.slots[self.active].bridge.reset();
        }
        self.active = index;
        self.fed = true;
        let mut result =
            self.slots[index]
                .bridge
                .push_packet(data.into(), RInputTransport::Iec61937, data_type);
        result.did_reset |= switched;
        result
    }

    /// The bridge for burst type `data_type`, asked once.
    fn iec_bridge(&mut self, data: &[u8], data_type: u8) -> Option<usize> {
        let entry = &mut self.iec_route[usize::from(data_type)];
        if *entry == IEC_UNPROBED {
            *entry = self
                .slots
                .iter()
                .position(|slot| {
                    slot.probe.is_some_and(|probe| {
                        probe(data.into(), RInputTransport::Iec61937, data_type).verdict
                            == RProbeVerdict::Claim
                    })
                })
                .map_or(IEC_NO_BRIDGE, |index| index as u8);
        }
        (*entry != IEC_NO_BRIDGE).then_some(usize::from(*entry))
    }

    fn push_raw(&mut self, data: &[u8]) -> RPushResult {
        match self.raw {
            RawRoute::Locked(index) => self.deliver(index, data),
            RawRoute::Probation(fallback) => self.push_on_probation(fallback, data),
            RawRoute::Undecided => self.push_undecided(data),
        }
    }

    fn deliver(&mut self, index: usize, data: &[u8]) -> RPushResult {
        self.active = index;
        self.fed = true;
        self.slots[index]
            .bridge
            .push_packet(data.into(), RInputTransport::Raw, 0)
    }

    /// No route yet: keep the bytes until a bridge claims a start.
    fn push_undecided(&mut self, data: &[u8]) -> RPushResult {
        let probes: Vec<Option<ProbeFn>> = self.slots.iter().map(|slot| slot.probe).collect();
        let mut rest = data;
        loop {
            let taken = self.undecided.append(rest);
            rest = &rest[taken..];
            if let Some((index, start)) = self.undecided.decide(&probes) {
                self.raw = RawRoute::Locked(index);
                let mut result = self.deliver_undecided(index, start);
                if !rest.is_empty() {
                    let more = self.deliver(index, rest);
                    merge(&mut result, more);
                }
                return result;
            }
            if rest.is_empty() {
                break;
            }
            // Full with nobody decided: let go of what holds it.
            self.undecided.make_room();
        }
        let dropped = self.undecided.take_dropped();
        if dropped > 0 {
            self.unrouted.note(dropped, || {
                "no decoder bridge recognises the start of this raw stream yet".to_owned()
            });
        }
        empty_result()
    }

    /// The decided bridge receives the kept bytes from its claimed start.
    fn deliver_undecided(&mut self, index: usize, start: usize) -> RPushResult {
        let bytes = std::mem::take(&mut self.undecided.buf);
        let dropped = self.undecided.take_dropped() + start;
        if dropped > 0 {
            log::warn!(
                "{dropped} raw bytes before the {} stream start were dropped",
                self.slot_name(index)
            );
        }
        let result = self.deliver(index, &bytes[start.min(bytes.len())..]);
        self.undecided.buf = bytes;
        self.undecided.clear();
        result
    }

    /// After a reset: a push that starts another bridge's stream at its first
    /// byte moves the route there; one that may (a header cut short) is held
    /// until that is decided; anything else goes to the fallback bridge.
    fn push_on_probation(&mut self, fallback: usize, data: &[u8]) -> RPushResult {
        let holding = !self.undecided.buf.is_empty();
        // What the probes judge: the held bytes topped up to the bound, or
        // this push up to the bound. `rest` is what lies past it, which goes
        // wherever the judged bytes go.
        let judged = if holding {
            self.undecided.append(data)
        } else {
            data.len().min(MAX_UNDECIDED_RAW)
        };
        let rest = if holding { &data[judged..] } else { &[][..] };
        let start = if holding {
            probe_first_byte(&self.slots, &self.undecided.buf)
        } else {
            probe_first_byte(&self.slots, &data[..judged])
        };
        let full = if holding {
            self.undecided.buf.len() >= MAX_UNDECIDED_RAW
        } else {
            data.len() >= MAX_UNDECIDED_RAW
        };
        let target = match start {
            FirstByte::Claimed(index) => {
                self.raw = RawRoute::Locked(index);
                index
            }
            FirstByte::Pending if !full => {
                if !holding {
                    self.undecided.buf.extend_from_slice(data);
                }
                return empty_result();
            }
            FirstByte::Pending => {
                // A start that does not complete within the bound is not one.
                log::warn!(
                    "a possible stream start after a reset was not confirmed within {} bytes; \
                     it resumes on bridge {}",
                    MAX_UNDECIDED_RAW,
                    fallback + 1
                );
                fallback
            }
            FirstByte::None => fallback,
        };
        let mut result = if holding {
            self.deliver_held(target)
        } else {
            self.deliver(target, data)
        };
        if !rest.is_empty() {
            let more = self.deliver(target, rest);
            merge(&mut result, more);
        }
        result
    }

    /// The held bytes, in one push, to `index`.
    fn deliver_held(&mut self, index: usize) -> RPushResult {
        let bytes = std::mem::take(&mut self.undecided.buf);
        let result = self.deliver(index, &bytes);
        self.undecided.buf = bytes;
        self.undecided.buf.clear();
        result
    }

    /// Reset every bridge. The bridge that had the stream stays its fallback
    /// (raw input), so a seek resumes on it; a codec named by `input_codec`
    /// keeps its bridge.
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.bridge.reset();
        }
        self.raw = match (self.forced, self.raw) {
            (Some(forced), _) => RawRoute::Locked(forced),
            (None, RawRoute::Locked(index) | RawRoute::Probation(index)) => {
                RawRoute::Probation(index)
            }
            (None, RawRoute::Undecided) => RawRoute::Undecided,
        };
        self.undecided.clear();
        self.unrouted = Unrouted::default();
        self.fed = false;
    }

    /// Send a configuration key. `input_codec` names the bridge raw input
    /// goes to (see [`BridgeLib::input_codecs`]); every other key goes to
    /// every bridge, and is accepted when one of them takes it.
    pub fn configure(&mut self, key: &str, value: &str) -> bool {
        if self.slots.len() == 1 {
            return self.slots[0].bridge.configure(key.into(), value.into());
        }
        if key == "input_codec" {
            return self.configure_input_codec(value);
        }
        let mut accepted = false;
        for slot in &mut self.slots {
            accepted |= slot.bridge.configure(key.into(), value.into());
        }
        accepted
    }

    fn configure_input_codec(&mut self, value: &str) -> bool {
        let codec = value.trim().to_ascii_lowercase();
        if codec.is_empty() || codec == "auto" {
            for slot in &mut self.slots {
                slot.bridge.configure("input_codec".into(), value.into());
            }
            if self.forced.take().is_some() {
                self.raw = RawRoute::Undecided;
            }
            return true;
        }
        let Some(index) = self
            .slots
            .iter()
            .position(|slot| slot.input_codecs.contains(&codec))
        else {
            log::warn!("no decoder bridge lists the codec '{value}'");
            return false;
        };
        if !self.slots[index]
            .bridge
            .configure("input_codec".into(), value.into())
        {
            return false;
        }
        self.forced = Some(index);
        self.raw = RawRoute::Locked(index);
        self.active = index;
        true
    }

    /// The modes of every bridge, each once, in load order: what the user
    /// may pick from. Not the list of values a bridge accepts.
    pub fn supported_drc_modes(&self) -> RVec<RString> {
        let mut modes: RVec<RString> = RVec::new();
        for slot in &self.slots {
            for mode in slot.bridge.supported_drc_modes() {
                if !modes.contains(&mode) {
                    modes.push(mode);
                }
            }
        }
        modes
    }

    /// Send the DRC mode to every bridge; in force when one takes it.
    pub fn set_drc_mode(&mut self, mode: &str) -> bool {
        let mut accepted = false;
        for slot in &mut self.slots {
            accepted |= slot.bridge.set_drc_mode(mode.into());
        }
        accepted
    }

    fn current(&self) -> &FormatBridgeBox {
        &self.slots[self.active].bridge
    }

    pub fn is_ready(&self) -> bool {
        self.current().is_ready()
    }

    pub fn has_objects(&self) -> bool {
        self.current().has_objects()
    }

    /// The format every bridge of the set agrees on (checked at open).
    pub fn coordinate_format(&self) -> RCoordinateFormat {
        self.slots[0].bridge.coordinate_format()
    }

    pub fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
        self.current().vbap_cartesian_defaults()
    }

    pub fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
        self.current().preferred_vbap_table_mode()
    }

    pub fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
        self.current().fixed_channel_poses()
    }

    pub fn source_family(&self) -> RString {
        self.current().source_family()
    }

    pub fn source_label(&self) -> RString {
        self.current().source_label()
    }

    pub fn channel_tags(&self) -> RVec<RChannelTag> {
        self.current().channel_tags()
    }

    fn slot_name(&self, index: usize) -> String {
        format!("bridge {}", index + 1)
    }
}

/// What the probes say about the first byte of a push.
enum FirstByte {
    /// This bridge's stream starts there (the first to claim, in load order).
    Claimed(usize),
    /// A start there is possible, but its header is not complete yet.
    Pending,
    None,
}

/// Ask every bridge whether `bytes` starts its stream at offset 0, showing
/// each [`PROBATION_WINDOW`] bytes, or as many as it asks for, so that a
/// probe of a long stream scans no more than a header.
fn probe_first_byte(slots: &[Slot], bytes: &[u8]) -> FirstByte {
    let mut pending = false;
    for (index, slot) in slots.iter().enumerate() {
        let Some(probe) = slot.probe else { continue };
        let mut shown = bytes.len().min(PROBATION_WINDOW);
        loop {
            let answer = probe(bytes[..shown].into(), RInputTransport::Raw, 0);
            if answer.offset != 0 {
                break;
            }
            match answer.verdict {
                // A bridge loaded earlier that may also start here goes first.
                RProbeVerdict::Claim if pending => return FirstByte::Pending,
                RProbeVerdict::Claim => return FirstByte::Claimed(index),
                RProbeVerdict::Pending => {
                    let needed = answer.needed as usize;
                    if needed > shown && needed <= bytes.len() {
                        shown = needed;
                        continue;
                    }
                    if needed > bytes.len() {
                        pending = true;
                    }
                    break;
                }
                RProbeVerdict::None => break,
            }
        }
    }
    if pending {
        FirstByte::Pending
    } else {
        FirstByte::None
    }
}

/// What a bridge has said about the undecided bytes, in buffer offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Answer {
    /// Not asked yet.
    Unknown,
    Claim(usize),
    Pending(usize),
    /// No start of this bridge's before this offset.
    RuledOut(usize),
}

/// One bridge's progress through the undecided bytes.
#[derive(Clone, Copy, Debug)]
struct Scan {
    answer: Answer,
    /// Where its next probe starts.
    from: usize,
    /// The end of the last window it was shown.
    seen_end: usize,
    /// The end of the bytes its pending candidate asked for; 0 for none.
    need_end: usize,
    /// The buffer length before which it is not asked again.
    wait_until: usize,
}

impl Default for Scan {
    fn default() -> Self {
        Self {
            answer: Answer::Unknown,
            from: 0,
            seen_end: 0,
            need_end: 0,
            wait_until: 0,
        }
    }
}

impl Scan {
    /// Show `probe` the bytes it has not judged yet, a window at a time,
    /// until it claims a start, waits for more bytes, or has seen them all.
    fn ask(&mut self, probe: ProbeFn, buf: &[u8]) {
        let len = buf.len();
        while !matches!(self.answer, Answer::Claim(_)) && self.from < len && len >= self.wait_until
        {
            let end = len
                .min(self.from.max(self.seen_end).saturating_add(PROBE_WINDOW))
                .max(len.min(self.need_end));
            let answer = probe(buf[self.from..end].into(), RInputTransport::Raw, 0);
            let at = (self.from + answer.offset as usize).min(end);
            self.seen_end = end;
            match answer.verdict {
                RProbeVerdict::Claim => self.answer = Answer::Claim(at),
                RProbeVerdict::Pending => {
                    self.answer = Answer::Pending(at);
                    self.from = at;
                    self.need_end = at.saturating_add(answer.needed as usize);
                    // Asked again once it has what it asked for; a probe that
                    // asks for no more than it saw waits for new bytes.
                    self.wait_until = self.need_end.max(end + 1);
                }
                RProbeVerdict::None => {
                    self.answer = Answer::RuledOut(at);
                    self.from = self.from.max(at);
                    self.need_end = 0;
                    self.wait_until = end + 1;
                }
            }
        }
    }

    /// The first `count` bytes left the buffer.
    fn shift(&mut self, count: usize) {
        let shift = |at: usize| at.saturating_sub(count);
        self.answer = match self.answer {
            Answer::Unknown => Answer::Unknown,
            Answer::Claim(at) => Answer::Claim(shift(at)),
            Answer::Pending(at) => Answer::Pending(shift(at)),
            Answer::RuledOut(at) => Answer::RuledOut(shift(at)),
        };
        self.from = shift(self.from);
        self.seen_end = shift(self.seen_end);
        self.need_end = shift(self.need_end);
        self.wait_until = shift(self.wait_until);
    }
}

/// The raw bytes no bridge has claimed yet, and where each bridge stands.
#[derive(Default)]
struct Undecided {
    buf: Vec<u8>,
    scans: Vec<Scan>,
    /// Bytes every bridge ruled out, dropped since the last report.
    dropped: usize,
}

impl Undecided {
    fn new(bridges: usize) -> Self {
        Self {
            buf: Vec::with_capacity(MAX_UNDECIDED_RAW),
            scans: vec![Scan::default(); bridges],
            dropped: 0,
        }
    }

    fn clear(&mut self) {
        self.buf.clear();
        self.scans.fill(Scan::default());
    }

    fn take_dropped(&mut self) -> usize {
        std::mem::take(&mut self.dropped)
    }

    /// Append as much of `data` as fits under [`MAX_UNDECIDED_RAW`]; how
    /// much that was.
    fn append(&mut self, data: &[u8]) -> usize {
        let taken = data.len().min(MAX_UNDECIDED_RAW - self.buf.len());
        self.buf.extend_from_slice(&data[..taken]);
        taken
    }

    /// Ask the bridges that can answer anew, then decide if a start is
    /// settled: the earliest claim that no bridge can still precede. Drops
    /// the bytes every bridge has ruled out.
    fn decide(&mut self, probes: &[Option<ProbeFn>]) -> Option<(usize, usize)> {
        for (scan, probe) in self.scans.iter_mut().zip(probes) {
            match probe {
                Some(probe) => scan.ask(*probe, &self.buf),
                None => scan.answer = Answer::RuledOut(usize::MAX),
            }
        }
        let decision = self.settled_claim();
        if decision.is_none() {
            self.drop_ruled_out();
        }
        decision
    }

    /// The earliest claim, when every other bridge has answered later, or
    /// ruled out every byte up to it; load order breaks a tie.
    fn settled_claim(&self) -> Option<(usize, usize)> {
        let (start, winner) = self
            .scans
            .iter()
            .enumerate()
            .filter_map(|(index, scan)| match scan.answer {
                Answer::Claim(at) => Some((at, index)),
                _ => None,
            })
            .min()?;
        let settled = self
            .scans
            .iter()
            .enumerate()
            .all(|(index, scan)| match scan.answer {
                _ if index == winner => true,
                Answer::Claim(at) => at > start || (at == start && index > winner),
                Answer::Pending(at) | Answer::RuledOut(at) => at > start,
                Answer::Unknown => false,
            });
        settled.then_some((winner, start))
    }

    /// Drop the leading bytes every bridge has ruled out.
    fn drop_ruled_out(&mut self) {
        let keep_from = self
            .scans
            .iter()
            .map(|scan| match scan.answer {
                Answer::Unknown => 0,
                Answer::Claim(at) | Answer::Pending(at) | Answer::RuledOut(at) => at,
            })
            .min()
            .unwrap_or(0)
            .min(self.buf.len());
        if keep_from == 0 {
            return;
        }
        self.buf.drain(..keep_from);
        self.dropped += keep_from;
        for scan in &mut self.scans {
            scan.shift(keep_from);
        }
    }

    /// The buffer is full and undecided: abandon pending starts, earliest
    /// first, which is what holds it, until a claim they held back is settled
    /// (left to the caller's next decision) or the abandoned bytes free room.
    /// If nothing does, drop the older half and start the scans over.
    fn make_room(&mut self) {
        while self.settled_claim().is_none() {
            let holder = self
                .scans
                .iter()
                .enumerate()
                .filter_map(|(index, scan)| match scan.answer {
                    Answer::Pending(at) => Some((at, index)),
                    _ => None,
                })
                .min();
            let Some((at, index)) = holder else { break };
            log::warn!(
                "a possible stream start for bridge {} was not confirmed within {} bytes; \
                 it is abandoned",
                index + 1,
                MAX_UNDECIDED_RAW
            );
            let scan = &mut self.scans[index];
            scan.answer = Answer::RuledOut(at + 1);
            scan.from = at + 1;
            scan.need_end = 0;
            scan.wait_until = 0;
            if self.settled_claim().is_some() {
                return;
            }
            self.drop_ruled_out();
            if self.buf.len() < MAX_UNDECIDED_RAW {
                return;
            }
        }
        if self.settled_claim().is_some() {
            return;
        }
        if self.buf.len() >= MAX_UNDECIDED_RAW {
            let half = MAX_UNDECIDED_RAW / 2;
            self.buf.drain(..half);
            self.dropped += half;
            self.scans.fill(Scan::default());
        }
    }
}

/// Rate-limited warning for input no bridge takes: the first time in a
/// stream, then at every doubling of the bytes lost.
#[derive(Default)]
struct Unrouted {
    bytes: u64,
    next_report: u64,
}

impl Unrouted {
    fn note(&mut self, bytes: usize, what: impl FnOnce() -> String) {
        self.bytes += bytes as u64;
        if self.bytes > self.next_report {
            log::warn!("{} ({} bytes so far)", what(), self.bytes);
            self.next_report = self.bytes.saturating_mul(2);
        }
    }
}

fn empty_result() -> RPushResult {
    RPushResult {
        frames: RVec::new(),
        error_message: RString::new(),
        did_reset: false,
    }
}

/// `more` appended to `result`: two pushes reported as one.
fn merge(result: &mut RPushResult, more: RPushResult) {
    result.frames.extend(more.frames);
    if result.error_message.is_empty() {
        result.error_message = more.error_message;
    }
    result.did_reset |= more.did_reset;
}

#[cfg(test)]
mod tests {
    use super::*;
    use abi_stable::sabi_trait::prelude::TD_Opaque;
    use abi_stable::std_types::RStr;
    use bridge_api::{FormatBridge, FormatBridge_TO};
    use std::cell::Cell;
    use std::sync::{Arc, Mutex};

    /// What a test bridge was sent.
    #[derive(Default)]
    struct Log {
        bytes: Vec<u8>,
        bursts: Vec<u8>,
        resets: usize,
        configured: Vec<(String, String)>,
        drc: Vec<String>,
    }

    struct TestBridge {
        log: Arc<Mutex<Log>>,
        format: RCoordinateFormat,
        drc_modes: &'static [&'static str],
    }

    impl FormatBridge for TestBridge {
        fn push_packet(
            &mut self,
            data: RSlice<'_, u8>,
            transport: RInputTransport,
            data_type: u8,
        ) -> RPushResult {
            let mut log = self.log.lock().unwrap();
            match transport {
                RInputTransport::Raw => log.bytes.extend_from_slice(data.as_slice()),
                RInputTransport::Iec61937 => log.bursts.push(data_type),
            }
            empty_result()
        }
        fn reset(&mut self) {
            self.log.lock().unwrap().resets += 1;
        }
        fn is_ready(&self) -> bool {
            true
        }
        fn has_objects(&self) -> bool {
            false
        }
        fn configure(&mut self, key: RStr<'_>, value: RStr<'_>) -> bool {
            self.log
                .lock()
                .unwrap()
                .configured
                .push((key.to_string(), value.to_string()));
            key.as_str() != "refused"
        }
        fn coordinate_format(&self) -> RCoordinateFormat {
            self.format
        }
        fn vbap_cartesian_defaults(&self) -> RVbapCartesianDefaults {
            RVbapCartesianDefaults::BALANCED
        }
        fn preferred_vbap_table_mode(&self) -> RVbapTableMode {
            RVbapTableMode::Cartesian
        }
        fn supported_drc_modes(&self) -> RVec<RString> {
            self.drc_modes.iter().map(|m| RString::from(*m)).collect()
        }
        fn set_drc_mode(&mut self, mode: RStr<'_>) -> bool {
            self.log.lock().unwrap().drc.push(mode.to_string());
            self.drc_modes.contains(&mode.as_str())
        }
        fn fixed_channel_poses(&self) -> RVec<RChannelPose> {
            RVec::new()
        }
    }

    fn test_bridge(log: &Arc<Mutex<Log>>) -> FormatBridgeBox {
        format_bridge(log, RCoordinateFormat::Cartesian, &[])
    }

    fn format_bridge(
        log: &Arc<Mutex<Log>>,
        format: RCoordinateFormat,
        drc_modes: &'static [&'static str],
    ) -> FormatBridgeBox {
        FormatBridge_TO::from_value(
            TestBridge {
                log: Arc::clone(log),
                format,
                drc_modes,
            },
            TD_Opaque,
        )
    }

    thread_local! {
        /// Bytes shown to the test probes on this thread.
        static PROBED: Cell<usize> = const { Cell::new(0) };
    }

    /// A stream of bridge `tag` starts with `tag` four times, then
    /// `extra` more header bytes; the claim waits for all of them. Scans the
    /// window for the earliest start, as a real probe does.
    fn probe_for(tag: u8, extra: usize, data: &[u8]) -> RProbe {
        PROBED.with(|p| p.set(p.get() + data.len()));
        let header = 4 + extra;
        for start in 0..data.len() {
            let rest = &data[start..];
            let sync = rest.len().min(4);
            if rest[..sync].iter().all(|&b| b == tag) {
                if rest.len() >= header {
                    return RProbe::claim(start as u32);
                }
                return RProbe::pending(start as u32, header as u32);
            }
        }
        RProbe::none(data.len() as u32)
    }

    extern "C" fn probe_a(data: RSlice<'_, u8>, transport: RInputTransport, dt: u8) -> RProbe {
        match transport {
            RInputTransport::Raw => probe_for(b'A', 0, data.as_slice()),
            RInputTransport::Iec61937 if dt == 0x15 => RProbe::claim(0),
            RInputTransport::Iec61937 => RProbe::none(0),
        }
    }

    extern "C" fn probe_b(data: RSlice<'_, u8>, transport: RInputTransport, dt: u8) -> RProbe {
        match transport {
            RInputTransport::Raw => probe_for(b'B', 0, data.as_slice()),
            RInputTransport::Iec61937 if dt == 0x0B => RProbe::claim(0),
            RInputTransport::Iec61937 => RProbe::none(0),
        }
    }

    /// Like A, with a long header: a start stays pending for 10 bytes.
    extern "C" fn probe_long_a(data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RProbe {
        probe_for(b'A', 6, data.as_slice())
    }

    /// Never decides a start it sees: pending forever at the first `C`.
    extern "C" fn probe_stuck(data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RProbe {
        PROBED.with(|p| p.set(p.get() + data.len()));
        match data.iter().position(|&b| b == b'C') {
            Some(at) => RProbe::pending(at as u32, u32::MAX),
            None => RProbe::none(data.len() as u32),
        }
    }

    /// A `C` stream with a 4-byte header, like A and B.
    extern "C" fn probe_c(data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RProbe {
        probe_for(b'C', 0, data.as_slice())
    }

    extern "C" fn probe_never(data: RSlice<'_, u8>, _: RInputTransport, _: u8) -> RProbe {
        PROBED.with(|p| p.set(p.get() + data.len()));
        RProbe::none(data.len().saturating_sub(3) as u32)
    }

    /// Bridges A and B, with what each received.
    fn set_ab() -> (BridgeSet, Arc<Mutex<Log>>, Arc<Mutex<Log>>) {
        let (a, b) = (Arc::default(), Arc::default());
        let set = BridgeSet::from_parts(vec![
            (probe_a as ProbeFn, vec!["acodec".into()], test_bridge(&a)),
            (probe_b as ProbeFn, vec!["bcodec".into()], test_bridge(&b)),
        ])
        .unwrap();
        (set, a, b)
    }

    fn push_raw(set: &mut BridgeSet, data: &[u8]) {
        set.push_packet(data, RInputTransport::Raw, 0);
    }

    fn bytes(log: &Arc<Mutex<Log>>) -> Vec<u8> {
        log.lock().unwrap().bytes.clone()
    }

    #[test]
    fn a_single_bridge_takes_everything_unprobed() {
        let log = Arc::default();
        let mut set = BridgeSet::single(test_bridge(&log));
        push_raw(&mut set, b"xyz");
        set.push_packet(b"", RInputTransport::Iec61937, 0x42);
        assert_eq!(bytes(&log), b"xyz");
        assert_eq!(log.lock().unwrap().bursts, [0x42]);
    }

    #[test]
    fn raw_input_goes_to_the_bridge_whose_start_comes_first() {
        let (mut set, a, b) = set_ab();
        push_raw(&mut set, b"..junk..BBBBpayload");
        push_raw(&mut set, b"AAAAmore");
        assert_eq!(bytes(&b), b"BBBBpayloadAAAAmore");
        assert!(bytes(&a).is_empty());
    }

    #[test]
    fn the_route_does_not_depend_on_how_the_reads_cut_the_stream() {
        let stream = b"xxAAAApayload-BBBB-more";
        for cut in 1..stream.len() {
            let (mut set, a, b) = set_ab();
            push_raw(&mut set, &stream[..cut]);
            push_raw(&mut set, &stream[cut..]);
            assert_eq!(bytes(&a), &stream[2..], "cut at {cut}");
            assert!(bytes(&b).is_empty(), "cut at {cut}");
        }
        let (mut set, a, _) = set_ab();
        for byte in stream {
            push_raw(&mut set, std::slice::from_ref(byte));
        }
        assert_eq!(bytes(&a), &stream[2..]);
    }

    #[test]
    fn a_pending_earlier_start_holds_a_later_claim() {
        // A needs 10 header bytes; B's start at 4 is complete at 8, first.
        let (a, b) = (Arc::default(), Arc::default());
        let parts = || {
            vec![
                (probe_long_a as ProbeFn, vec![], test_bridge(&a)),
                (probe_b as ProbeFn, vec![], test_bridge(&b)),
            ]
        };
        let stream = b"AAAABBBB-rest";
        let mut set = BridgeSet::from_parts(parts()).unwrap();
        for byte in stream {
            push_raw(&mut set, std::slice::from_ref(byte));
        }
        assert_eq!(bytes(&a), stream);
        assert!(bytes(&b).is_empty());
    }

    #[test]
    fn a_tie_goes_to_the_bridge_loaded_first() {
        let (first, second) = (Arc::default(), Arc::default());
        let mut set = BridgeSet::from_parts(vec![
            (probe_b as ProbeFn, vec![], test_bridge(&first)),
            (probe_b as ProbeFn, vec![], test_bridge(&second)),
        ])
        .unwrap();
        push_raw(&mut set, b"BBBB");
        assert_eq!(bytes(&first), b"BBBB");
        assert!(bytes(&second).is_empty());
    }

    #[test]
    fn a_start_that_never_confirms_is_abandoned_at_the_bound() {
        let (stuck, b) = (Arc::default(), Arc::default());
        let mut set = BridgeSet::from_parts(vec![
            (probe_stuck as ProbeFn, vec![], test_bridge(&stuck)),
            (probe_b as ProbeFn, vec![], test_bridge(&b)),
        ])
        .unwrap();
        push_raw(&mut set, b"C");
        let filler = vec![b'.'; 4096];
        for _ in 0..(MAX_UNDECIDED_RAW / filler.len() + 1) {
            push_raw(&mut set, &filler);
        }
        push_raw(&mut set, b"BBBBtail");
        assert!(bytes(&stuck).is_empty());
        assert_eq!(bytes(&b), b"BBBBtail");
    }

    #[test]
    fn probing_costs_a_bounded_amount_per_byte_received() {
        let logs: [Arc<Mutex<Log>>; 2] = Default::default();
        let mut set = BridgeSet::from_parts(vec![
            (probe_never as ProbeFn, vec![], test_bridge(&logs[0])),
            (probe_stuck as ProbeFn, vec![], test_bridge(&logs[1])),
        ])
        .unwrap();
        PROBED.with(|p| p.set(0));
        let input = 1 << 20;
        for i in 0..input {
            // A plausible start every 10 KiB, pending until the bound.
            let byte = if i % 10_240 == 0 { b'C' } else { b'.' };
            push_raw(&mut set, &[byte]);
        }
        let shown = PROBED.with(Cell::get);
        assert!(
            shown <= 6 * input,
            "{shown} bytes shown to the probes for {input} received"
        );
    }

    #[test]
    fn iec_bursts_go_by_type_and_a_switch_resets_the_old_bridge() {
        let (mut set, a, b) = set_ab();
        let first = set.push_packet(b"", RInputTransport::Iec61937, 0x15);
        assert!(!first.did_reset);
        let switched = set.push_packet(b"", RInputTransport::Iec61937, 0x0B);
        assert!(switched.did_reset);
        let steady = set.push_packet(b"", RInputTransport::Iec61937, 0x0B);
        assert!(!steady.did_reset);
        let unknown = set.push_packet(b"", RInputTransport::Iec61937, 0x07);
        assert!(unknown.frames.is_empty());
        assert_eq!(a.lock().unwrap().bursts, [0x15]);
        assert_eq!(a.lock().unwrap().resets, 1);
        assert_eq!(b.lock().unwrap().bursts, [0x0B, 0x0B]);
    }

    /// The first bridge is active only by default until a packet arrives: a
    /// first burst for another bridge, at start or after a seek, is no
    /// switch.
    #[test]
    fn a_first_burst_for_a_later_bridge_is_no_switch() {
        let (mut set, a, b) = set_ab();
        let first = set.push_packet(b"", RInputTransport::Iec61937, 0x0B);
        assert!(!first.did_reset);
        assert_eq!(a.lock().unwrap().resets, 0);
        set.reset();
        let resets = a.lock().unwrap().resets;
        let after_seek = set.push_packet(b"", RInputTransport::Iec61937, 0x15);
        assert!(!after_seek.did_reset);
        assert_eq!(a.lock().unwrap().resets, resets);
        let switched = set.push_packet(b"", RInputTransport::Iec61937, 0x0B);
        assert!(switched.did_reset);
        assert_eq!(b.lock().unwrap().bursts, [0x0B, 0x0B]);
    }

    #[test]
    fn after_a_seek_headerless_data_resumes_on_the_last_bridge() {
        let (mut set, a, b) = set_ab();
        push_raw(&mut set, b"BBBBstream");
        set.reset();
        push_raw(&mut set, b"continued");
        assert_eq!(bytes(&b), b"BBBBstreamcontinued");
        assert!(bytes(&a).is_empty());
    }

    #[test]
    fn after_a_seek_a_new_start_moves_the_route_whatever_the_cut() {
        let next = b"AAAAnext stream";
        for cut in 1..next.len() {
            let (mut set, a, b) = set_ab();
            push_raw(&mut set, b"BBBBfirst");
            set.reset();
            push_raw(&mut set, &next[..cut]);
            push_raw(&mut set, &next[cut..]);
            assert_eq!(bytes(&a), next, "cut at {cut}");
            assert_eq!(bytes(&b), b"BBBBfirst", "cut at {cut}");
        }
    }

    #[test]
    fn after_a_seek_a_false_start_goes_back_to_the_last_bridge() {
        let (mut set, a, b) = set_ab();
        push_raw(&mut set, b"BBBBfirst");
        set.reset();
        push_raw(&mut set, b"AA");
        push_raw(&mut set, b"xyz");
        push_raw(&mut set, b"tail");
        assert!(bytes(&a).is_empty());
        assert_eq!(bytes(&b), b"BBBBfirstAAxyztail");
    }

    #[test]
    fn after_a_seek_a_cut_header_completed_by_a_large_push_moves_the_route() {
        let (mut set, a, b) = set_ab();
        push_raw(&mut set, b"BBBBfirst");
        set.reset();
        push_raw(&mut set, b"AA");
        let mut large = b"AA".to_vec();
        large.resize(2 + MAX_UNDECIDED_RAW, b'.');
        push_raw(&mut set, &large);
        let mut expected = b"AA".to_vec();
        expected.extend_from_slice(&large);
        assert_eq!(bytes(&a), expected);
        assert_eq!(bytes(&b), b"BBBBfirst");
    }

    #[test]
    fn after_a_seek_a_held_start_never_exceeds_the_bound() {
        let (stuck, b) = (Arc::default(), Arc::default());
        let mut set = BridgeSet::from_parts(vec![
            (probe_stuck as ProbeFn, vec![], test_bridge(&stuck)),
            (probe_b as ProbeFn, vec![], test_bridge(&b)),
        ])
        .unwrap();
        push_raw(&mut set, b"BBBBfirst");
        set.reset();
        let mut large = vec![b'.'; 2 * MAX_UNDECIDED_RAW];
        large[0] = b'C';
        push_raw(&mut set, &large);
        assert!(set.undecided.buf.capacity() <= MAX_UNDECIDED_RAW);
        assert!(set.undecided.buf.is_empty());
        let mut expected = b"BBBBfirst".to_vec();
        expected.extend_from_slice(&large);
        assert_eq!(bytes(&b), expected);
        assert!(bytes(&stuck).is_empty());
    }

    #[test]
    fn a_claim_held_back_by_an_abandoned_start_still_wins() {
        let (stuck, c) = (Arc::default(), Arc::default());
        let mut set = BridgeSet::from_parts(vec![
            (probe_stuck as ProbeFn, vec![], test_bridge(&stuck)),
            (probe_c as ProbeFn, vec![], test_bridge(&c)),
        ])
        .unwrap();
        let mut stream = b"CCCC".to_vec();
        stream.resize(4 + MAX_UNDECIDED_RAW, b'.');
        push_raw(&mut set, &stream);
        assert_eq!(bytes(&c), stream);
        assert!(bytes(&stuck).is_empty());
    }

    #[test]
    fn a_claim_held_back_by_several_abandoned_starts_still_wins() {
        let logs: [Arc<Mutex<Log>>; 3] = Default::default();
        let mut set = BridgeSet::from_parts(vec![
            (probe_stuck as ProbeFn, vec![], test_bridge(&logs[0])),
            (probe_stuck as ProbeFn, vec![], test_bridge(&logs[1])),
            (probe_c as ProbeFn, vec![], test_bridge(&logs[2])),
        ])
        .unwrap();
        let mut stream = b"CCCC".to_vec();
        stream.resize(4 + MAX_UNDECIDED_RAW, b'.');
        push_raw(&mut set, &stream);
        assert_eq!(bytes(&logs[2]), stream);
        assert!(bytes(&logs[0]).is_empty() && bytes(&logs[1]).is_empty());
    }

    #[test]
    fn input_codec_routes_raw_input_unprobed() {
        let (mut set, a, b) = set_ab();
        assert!(set.configure("input_codec", "BCODEC"));
        push_raw(&mut set, b"AAAA-not-probed");
        assert_eq!(bytes(&b), b"AAAA-not-probed");
        assert!(bytes(&a).is_empty());
        assert!(!set.configure("input_codec", "unknown"));
        set.reset();
        push_raw(&mut set, b"AAAA");
        assert_eq!(bytes(&b), b"AAAA-not-probedAAAA");
    }

    #[test]
    fn other_keys_and_drc_modes_go_to_every_bridge() {
        let (a, b) = (Arc::default(), Arc::default());
        let mut set = BridgeSet::from_parts(vec![
            (
                probe_a as ProbeFn,
                vec![],
                format_bridge(&a, RCoordinateFormat::Cartesian, &["Off", "Heavy"]),
            ),
            (
                probe_b as ProbeFn,
                vec![],
                format_bridge(&b, RCoordinateFormat::Cartesian, &["Off"]),
            ),
        ])
        .unwrap();
        assert!(set.configure("log_level", "debug"));
        assert!(!set.configure("refused", "x"));
        let modes: Vec<String> = set
            .supported_drc_modes()
            .into_iter()
            .map(RString::into_string)
            .collect();
        assert_eq!(modes, ["Off", "Heavy"]);
        assert!(set.set_drc_mode("Heavy"));
        assert!(!set.set_drc_mode("Standard"));
        for log in [&a, &b] {
            let log = log.lock().unwrap();
            assert_eq!(log.configured[0], ("log_level".into(), "debug".into()));
            assert_eq!(log.drc, ["Heavy", "Standard"]);
        }
    }

    #[test]
    fn bridges_of_different_coordinate_formats_are_refused() {
        let (a, b) = (Arc::default(), Arc::default());
        let refused = BridgeSet::from_parts(vec![
            (
                probe_a as ProbeFn,
                vec![],
                format_bridge(&a, RCoordinateFormat::Cartesian, &[]),
            ),
            (
                probe_b as ProbeFn,
                vec![],
                format_bridge(&b, RCoordinateFormat::Polar, &[]),
            ),
        ]);
        let err = format!("{:#}", refused.err().expect("refused"));
        assert!(err.contains("coordinate format"), "{err}");
    }
}
