//! The bed's own scene markers, one per channel, while nothing is playing.
//!
//! The channel editor is a table of positions, and a table of positions is not
//! a room. These markers are what makes the bed something to look at and drag:
//! Studio's own scene objects, not renderer metadata and not synthesised audio.
//! They exist so the bed can be seen and edited at rest, and they stand down as
//! soon as a stream owns the scene.
//!
//! Standing *back up* is the part a frame could not do. The hand-over is a
//! deadline — so long after the last spatial frame, the stream is over — and a
//! deadline checked while drawing is only checked while something is being
//! drawn. With the film stopped and the pointer still, the markers used to stay
//! away until the user moved the mouse.

use std::time::{Duration, Instant};

use super::Tick;
use crate::host::channels::{Channel, effective_channels};
use crate::host::commands::SharedState;
use crate::model::app_state::SourcePosition;
use crate::osc::dispatch::Live;

/// How long after the last spatial frame the session still counts as playing.
/// It covers the brief gap after a seek, so the markers do not flash back in
/// between two parts of the same stream.
const STREAM_IDLE: Duration = Duration::from_millis(800);

#[derive(Default)]
pub struct VirtualBed {
    /// What the published set was built from, so an unchanged bed is left
    /// alone.
    signature: Option<u64>,
    /// The ids this service put in the registry.
    ids: Vec<String>,
}

impl VirtualBed {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let mut guard = state.inner.lock().unwrap();
        let live = &mut *guard;
        // The catalogue is the same input as the editor's, digested once: both
        // must name the same channels or the markers would label themselves
        // differently from the rows.
        live.channels.refresh(&live.app);
        if let Some(at) = live.last_spatial_frame_at {
            let until = at + STREAM_IDLE;
            if now < until {
                // The live stream owns the scene.
                return Tick {
                    changed: self.clear(live),
                    next: Some(until),
                };
            }
        }
        let channels = effective_channels(&live.channels, &live.app);
        let signature = bed_signature(&channels);
        if self.signature == Some(signature) && self.ids.len() == channels.len() {
            return Tick::idle();
        }
        self.signature = Some(signature);
        self.publish(live, &channels);
        Tick {
            changed: true,
            next: None,
        }
    }

    /// Take the markers back out. Returns whether there were any.
    fn clear(&mut self, live: &mut Live) -> bool {
        if self.ids.is_empty() {
            return false;
        }
        for id in self.ids.drain(..) {
            live.app.sources.remove(&id);
        }
        self.signature = None;
        true
    }

    fn publish(&mut self, live: &mut Live, channels: &[Channel]) {
        // Live objects get no removal message when the engine simply stops
        // emitting them; left in place they would double the markers below.
        // The injected test source is neither live nor a bed channel — only its
        // own switch may remove it.
        let stale: Vec<String> = live
            .app
            .sources
            .keys()
            .filter(|id| {
                id.as_str() != super::object_test::SOURCE_ID
                    && !channels.iter().any(|c| &c.name == *id)
            })
            .cloned()
            .collect();
        for id in stale {
            live.app.sources.remove(&id);
        }
        // Resolved before the registry is touched: the speaker list and the
        // sources live in the same `Live`.
        let direct: Vec<Option<u32>> = channels
            .iter()
            .map(|channel| {
                (!channel.spatialize)
                    .then(|| {
                        live.selected_speakers().iter().position(|s| {
                            live.channels.canonical(&live.app, &s.id).as_deref()
                                == Some(channel.name.as_str())
                        })
                    })
                    .flatten()
                    .map(|i| i as u32)
            })
            .collect();
        self.ids.clear();
        for (channel, direct_speaker_index) in channels.iter().zip(direct) {
            let source = SourcePosition {
                x: channel.x,
                y: channel.y,
                z: channel.z,
                coord_mode: Some(channel.coord_mode.as_str().to_owned()),
                azimuth_deg: Some(channel.azimuth),
                elevation_deg: Some(channel.elevation),
                distance_m: Some(channel.distance.max(0.01)),
                gain_db: Some(channel.gain_db.round() as i32),
                fixed: Some(true),
                label: Some(channel.name.clone()),
                name: Some(channel.name.clone()),
                direct_speaker_index,
                ..Default::default()
            };
            live.app.sources.insert(channel.name.clone(), source);
            self.ids.push(channel.name.clone());
        }
    }
}

/// Cheap change detector for the marker set, so the sweep above runs only when
/// the bed actually moved.
fn bed_signature(channels: &[Channel]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for c in channels {
        c.name.hash(&mut hasher);
        c.coord_mode.hash(&mut hasher);
        for v in [c.x, c.y, c.z, c.azimuth, c.elevation, c.distance, c.gain_db] {
            v.to_bits().hash(&mut hasher);
        }
        c.spatialize.hash(&mut hasher);
    }
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_markers_stand_down_for_a_stream_and_come_back_on_a_deadline() {
        let state = crate::host::commands::tests::state();
        let mut service = VirtualBed::default();
        let now = Instant::now();

        // At rest: the fallback bed is materialised, one marker per channel.
        let tick = service.tick(&state, now);
        assert!(tick.changed);
        assert!(tick.next.is_none());
        let published = state.inner.lock().unwrap().app.sources.len();
        assert!(published > 0);
        // A settled bed is left alone.
        assert!(!service.tick(&state, now).changed);

        // A spatial frame arrives: the stream owns the scene, and the service
        // says when to look again rather than waiting for someone to draw.
        state.inner.lock().unwrap().last_spatial_frame_at = Some(now);
        let tick = service.tick(&state, now);
        assert!(tick.changed);
        assert_eq!(tick.next, Some(now + STREAM_IDLE));
        assert!(state.inner.lock().unwrap().app.sources.is_empty());

        // Past the deadline, they come back by themselves.
        let tick = service.tick(&state, now + STREAM_IDLE);
        assert!(tick.changed);
        assert_eq!(state.inner.lock().unwrap().app.sources.len(), published);
    }
}
