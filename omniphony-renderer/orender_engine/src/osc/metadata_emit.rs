use anyhow::Result;
use rosc::{OscMessage, OscPacket, OscType};
use std::sync::atomic::Ordering;

use super::{ObjectMeta, ObjectSnapshot, OscSender};

impl OscSender {
    pub fn send_object_position(&self, object_id: u32, x: f32, y: f32, z: f32) -> Result<()> {
        let msg = OscMessage {
            addr: format!("/omniphony/object/{}", object_id),
            args: vec![OscType::Float(x), OscType::Float(y), OscType::Float(z)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    pub fn send_bed_config(&self, channel_count: u32) -> Result<()> {
        let msg = OscMessage {
            addr: "/omniphony/bed/config".to_string(),
            args: vec![OscType::Int(channel_count as i32)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    pub fn send_timestamp(&self, sample_pos: u64, seconds: f64) -> Result<()> {
        let msg = OscMessage {
            addr: "/omniphony/timestamp".to_string(),
            args: vec![OscType::Long(sample_pos as i64), OscType::Double(seconds)],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
        self.send_to_all(&bytes);
        Ok(())
    }

    pub fn send_object_frame(
        &mut self,
        sample_pos: u64,
        ramp_duration: u32,
        coordinate_format: i32,
        objects: &[ObjectMeta],
    ) -> Result<()> {
        let frame_msg = OscMessage {
            addr: "/omniphony/spatial/frame".to_string(),
            args: vec![
                OscType::Long(sample_pos as i64),
                OscType::Long(self.content_generation as i64),
                OscType::Int(objects.len() as i32),
                OscType::Int(coordinate_format),
            ],
        };
        let bytes = rosc::encoder::encode(&OscPacket::Message(frame_msg))?;
        self.send_to_all(&bytes);

        let prev_len = self.prev_objects.as_ref().map_or(0, |prev| prev.len());
        let force_full = self
            .prev_objects
            .as_ref()
            .map_or(true, |prev| prev.len() != objects.len())
            || self.force_full_next.swap(false, Ordering::Relaxed);

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
                    OscType::Long(self.content_generation as i64),
                    OscType::String(String::new()),
                ],
            };
            let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
            self.send_to_all(&bytes);

            // Emit a zeroed /size for stale objects so clients can clear their
            // displays.
            let size_msg = OscMessage {
                addr: format!("/omniphony/object/{}/size", stale_id),
                args: vec![
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Float(0.0),
                    OscType::Long(self.content_generation as i64),
                ],
            };
            let size_bytes = rosc::encoder::encode(&OscPacket::Message(size_msg))?;
            self.send_to_all(&size_bytes);

            // Reset the fixed/label meta of the stale slot too.
            let meta_msg = OscMessage {
                addr: format!("/omniphony/object/{}/meta", stale_id),
                args: vec![
                    OscType::Int(0),
                    OscType::String(String::new()),
                    OscType::Long(self.content_generation as i64),
                ],
            };
            let meta_bytes = rosc::encoder::encode(&OscPacket::Message(meta_msg))?;
            self.send_to_all(&meta_bytes);
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
                        OscType::Long(self.content_generation as i64),
                    ],
                };
                let meta_bytes = rosc::encoder::encode(&OscPacket::Message(meta_msg))?;
                self.send_to_all(&meta_bytes);
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
                        OscType::Int(obj.gain),
                        OscType::Float(obj.priority),
                        OscType::Int(ramp_duration as i32),
                        OscType::Long(self.content_generation as i64),
                        OscType::String(obj.name.clone()),
                    ],
                };
                let bytes = rosc::encoder::encode(&OscPacket::Message(msg))?;
                self.send_to_all(&bytes);
            }

            if size_changed {
                let size_msg = OscMessage {
                    addr: format!("/omniphony/object/{}/size", object_id),
                    args: vec![
                        OscType::Float(obj.size[0]),
                        OscType::Float(obj.size[1]),
                        OscType::Float(obj.size[2]),
                        OscType::Long(self.content_generation as i64),
                    ],
                };
                let size_bytes = rosc::encoder::encode(&OscPacket::Message(size_msg))?;
                self.send_to_all(&size_bytes);
            }
        }

        self.prev_objects = Some(objects.iter().map(ObjectSnapshot::from_meta).collect());
        Ok(())
    }

    pub fn bump_content_generation(&mut self) {
        self.content_generation = self.content_generation.saturating_add(1);
        self.prev_objects = None;
        self.force_full_next.store(true, Ordering::Relaxed);
    }

    /// Force the next object frame to re-emit every object's full state
    /// (positions + names), without changing the content generation. Used after
    /// a seek/reset: object frames are delta-encoded, and the virtual-bed poses
    /// are static, so without this the names/positions are never re-sent and a
    /// client that cleared its object slots on the discontinuity is left showing
    /// defaults (unnamed objects at the origin) even though rendering is correct.
    pub fn request_full_object_resend(&mut self) {
        self.prev_objects = None;
        self.force_full_next.store(true, Ordering::Relaxed);
    }
}
