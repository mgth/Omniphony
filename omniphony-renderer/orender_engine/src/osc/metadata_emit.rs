use anyhow::Result;
use rosc::{OscMessage, OscPacket, OscType};
use std::sync::atomic::{AtomicBool, Ordering};

use super::telemetry::{Event, ObjectFrame, Out};
use super::{ObjectMeta, ObjectSnapshot, OscSender};
use runtime_control::osc_contract;

impl OscSender {
    /// Queue `timestamp` for the telemetry thread.
    pub fn send_timestamp(&mut self, sample_pos: u64, seconds: f64) {
        let block = self.telemetry.block;
        self.telemetry.push(Event::Timestamp {
            block,
            sample_pos,
            seconds,
        });
    }

    /// Queue one object frame for the telemetry thread, which sends the
    /// objects that changed since the last frame it sent (all of them after a
    /// content change or a forced resend). The list is copied into one the
    /// thread handed back, reusing its strings.
    pub fn send_object_frame(
        &mut self,
        sample_pos: u64,
        ramp_duration: u32,
        coordinate_format: i32,
        objects: &[ObjectMeta],
    ) {
        let mut list = self.telemetry.object_list();
        objects.clone_into(&mut list);
        let frame = ObjectFrame {
            block: self.telemetry.block,
            sample_pos,
            ramp_duration,
            coordinate_format,
            generation: self.telemetry.generation,
            force_full: self.telemetry.force_full,
            objects: list,
        };
        if self.telemetry.push(Event::Objects(frame)) {
            self.telemetry.force_full = false;
        }
    }

    /// The content generation stamped on outgoing object frames; bumped when
    /// the content changes (segment start, bridge reset, stream end).
    pub fn content_generation(&self) -> u64 {
        self.telemetry.generation
    }

    pub fn bump_content_generation(&mut self) {
        self.telemetry.generation = self.telemetry.generation.saturating_add(1);
        self.telemetry.force_full = true;
    }

    /// Force the next object frame to re-emit every object's full state
    /// (positions + names), without changing the content generation. Used after
    /// a seek/reset: object frames are delta-encoded, and the virtual-bed poses
    /// are static, so without this the names/positions are never re-sent and a
    /// client that cleared its object slots on the discontinuity is left showing
    /// defaults (unnamed objects at the origin) even though rendering is correct.
    pub fn request_full_object_resend(&mut self) {
        self.telemetry.force_full = true;
    }
}

/// The objects last sent, on the telemetry thread: object frames are
/// delta-encoded against them.
#[derive(Default)]
pub(super) struct ObjectDeltas {
    prev_objects: Option<Vec<ObjectSnapshot>>,
    generation: u64,
}

impl ObjectDeltas {
    pub(super) fn emit(
        &mut self,
        out: &mut Out,
        frame: &ObjectFrame,
        force_full_next: &AtomicBool,
    ) -> Result<()> {
        let objects = frame.objects.as_slice();
        let ramp_duration = frame.ramp_duration;
        let coordinate_format = frame.coordinate_format;
        let block = frame.block;
        if frame.generation != self.generation {
            self.generation = frame.generation;
            self.prev_objects = None;
        }
        let frame_msg = OscMessage {
            addr: osc_contract::SPATIAL_FRAME.to_string(),
            args: vec![
                OscType::Long(frame.sample_pos as i64),
                OscType::Long(self.generation as i64),
                OscType::Int(objects.len() as i32),
                OscType::Int(coordinate_format),
            ],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(frame_msg))?;
        out.send_stream(block, &bytes);

        let prev_len = self.prev_objects.as_ref().map_or(0, |prev| prev.len());
        // The listener's flag is taken whatever else forces this frame, so a
        // registration does not cost a second full frame.
        let force_full = force_full_next.swap(false, Ordering::Relaxed)
            | frame.force_full
            | self
                .prev_objects
                .as_ref()
                .map_or(true, |prev| prev.len() != objects.len());

        for stale_id in objects.len()..prev_len {
            let suffix = self
                .prev_objects
                .as_ref()
                .and_then(|prev| prev.get(stale_id))
                .map(|obj| {
                    if obj.coord_mode.eq_ignore_ascii_case("cartesian") {
                        "xyz"
                    } else {
                        "aed"
                    }
                })
                .unwrap_or(if coordinate_format == 1 { "aed" } else { "xyz" });
            // /xyz | /aed: position + speaker + gain + priority + ramp + gen + name
            // (9 args; `divergence` slot retired — clients should read `/size`).
            let msg = OscMessage {
                addr: format!("/omniphony/object/{}/{}", stale_id, suffix),
                args: vec![
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Int(-1),
                    OscType::Int(-128),
                    OscType::Float(0.0),
                    OscType::Int(ramp_duration as i32),
                    OscType::Long(self.generation as i64),
                    OscType::String(String::new()),
                ],
            };
            let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
            out.send_stream(block, &bytes);

            // Emit a zeroed /size for stale objects so clients can clear their
            // displays.
            let size_msg = OscMessage {
                addr: format!("/omniphony/object/{}/size", stale_id),
                args: vec![
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Long(self.generation as i64),
                ],
            };
            let size_bytes = rosc::encoder::encode(&OscPacket::Message(size_msg))?;
            out.send_stream(block, &size_bytes);

            // Reset the fixed/label meta of the stale slot too.
            let meta_msg = OscMessage {
                addr: format!("/omniphony/object/{}/meta", stale_id),
                args: vec![
                    OscType::Int(0),
                    OscType::String(String::new()),
                    OscType::Long(self.generation as i64),
                ],
            };
            let meta_bytes = rosc::encoder::encode(&OscPacket::Message(meta_msg))?;
            out.send_stream(block, &meta_bytes);

            // Say it outright, after the zeroed triple above. A client that
            // understands this drops the object; one that does not still sees
            // the zeros and clears its display the old way.
            //
            // This is emitted here rather than at the call sites on purpose:
            // `send_object_frame` is the single emitter both hosts go through
            // (the FFI/mpv path in `engine.rs`, and the CLI decode path in
            // `cli/decode/*`), so the two cannot drift apart.
            let remove_msg = OscMessage {
                addr: format!(
                    "/omniphony/object/{}/{}",
                    stale_id,
                    osc_contract::OBJECT_REMOVE_SUFFIX
                ),
                args: vec![OscType::Long(self.generation as i64)],
            };
            let remove_bytes = rosc::encoder::encode(&OscPacket::Message(remove_msg))?;
            out.send_stream(block, &remove_bytes);
        }

        for (object_id, obj) in objects.iter().enumerate() {
            let prev = self.prev_objects.as_ref().and_then(|p| p.get(object_id));
            let position_changed = force_full || prev.map_or(true, |p| !p.matches_position(obj));
            let size_changed = force_full || prev.map_or(true, |p| !p.matches_size(obj));
            let meta_changed = force_full || prev.map_or(true, |p| !p.matches_meta(obj));

            // Sparse per-slot meta: fixed-channel flag + canonical label
            // ([Int fixed, String label, Long generation]). A dedicated
            // additive message — appending to the positional payload would
            // break older clients' trailing-arg heuristics.
            if meta_changed {
                let meta_msg = OscMessage {
                    addr: format!("/omniphony/object/{}/meta", object_id),
                    args: vec![
                        OscType::Int(i32::from(obj.fixed)),
                        OscType::String(obj.label.clone()),
                        OscType::Long(self.generation as i64),
                        // Appended, so a client reading the first three by
                        // index is unaffected. Empty for an ordinary object.
                        OscType::String(obj.kind.as_wire().to_string()),
                    ],
                };
                let meta_bytes = rosc::encoder::encode(&OscPacket::Message(meta_msg))?;
                out.send_stream(block, &meta_bytes);
            }

            if position_changed {
                let suffix = if obj.coord_mode.eq_ignore_ascii_case("cartesian") {
                    "xyz"
                } else {
                    "aed"
                };
                let msg = OscMessage {
                    addr: format!("/omniphony/object/{}/{}", object_id, suffix),
                    args: vec![
                        OscType::Float(obj.x),
                        OscType::Float(obj.y),
                        OscType::Float(obj.z),
                        OscType::Int(obj.direct_speaker_index.map(|v| v as i32).unwrap_or(-1)),
                        // The wire stays Int: the native mpv overlay parses this tag, and it
                        // is display-only — the editor reads the bed itself, not this frame.
                        OscType::Int(obj.gain.round() as i32),
                        OscType::Float(obj.priority),
                        OscType::Int(ramp_duration as i32),
                        OscType::Long(self.generation as i64),
                        OscType::String(obj.name.clone()),
                    ],
                };
                let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
                out.send_stream(block, &bytes);
            }

            if size_changed {
                let size_msg = OscMessage {
                    addr: format!("/omniphony/object/{}/size", object_id),
                    args: vec![
                        OscType::Float(obj.size[0]),
                        OscType::Float(obj.size[1]),
                        OscType::Float(obj.size[2]),
                        OscType::Long(self.generation as i64),
                    ],
                };
                let size_bytes = rosc::encoder::encode(&OscPacket::Message(size_msg))?;
                out.send_stream(block, &size_bytes);
            }
        }

        self.prev_objects = Some(objects.iter().map(ObjectSnapshot::from_meta).collect());
        Ok(())
    }
}
