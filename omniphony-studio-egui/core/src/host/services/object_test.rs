//! The injected test object's scene marker.
//!
//! Two separate things, as the panel's own comment puts it: the *feature* puts
//! a source in the room — visible, selectable, metered, placeable — and the
//! *transport* decides whether it makes any noise. Only the first concerns this
//! service. It publishes the source into the registry so that the sphere, the
//! label, the trail, the list row and the meter are the ones every other object
//! gets, and nothing has to know this one is Studio's.
//!
//! Where the marker sits is not the same question twice. The scene shows where
//! the source *is* — the renderer's reported position while it plays, the
//! placed one otherwise. The editor's sheet keeps showing the placed position:
//! those markers are the handle being dragged, and a handle that runs away
//! from the pointer is not a handle.

use std::time::Instant;

use super::Tick;
use crate::host::commands::SharedState;
use crate::model::app_state::{Meter, SourcePosition};

/// `control_object_mute` addresses objects by number; this one has a name, so
/// its mute is routed through the test message instead.
pub const SOURCE_ID: &str = "injection";

/// What the view is showing, in the marker's own terms.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct ObjectTestMarker {
    /// The feature is on, so the source belongs in the room.
    pub shown: bool,
    /// The transport is running, so the renderer is saying where the source
    /// actually is. Mute does not clear this: a muted test is still placed.
    pub playing: bool,
    /// Where the user put it.
    pub placed: [f64; 3],
}

/// What the view is showing. Called whenever it draws; the service only acts on
/// a change, and the clock is nudged so the change is not left waiting for the
/// next packet.
pub fn set_object_test_marker(state: &SharedState, marker: ObjectTestMarker) {
    {
        let mut live = state.inner.lock().unwrap();
        if live.object_test_marker == marker {
            return;
        }
        live.object_test_marker = marker;
    }
    (state.waker)();
}

#[derive(Default)]
pub struct ObjectTestSource;

impl ObjectTestSource {
    pub fn tick(&mut self, state: &SharedState, _now: Instant) -> Tick {
        let mut live = state.inner.lock().unwrap();
        let marker = live.object_test_marker;
        if !marker.shown {
            // Studio's own marker, so nothing echoes its removal back: taking
            // it out of the registry here is the whole of switching it off.
            let had = live.app.sources.remove(SOURCE_ID).is_some();
            let had = live.app.source_levels.remove(SOURCE_ID).is_some() || had;
            return Tick {
                changed: had,
                next: None,
            };
        }
        let reported = live.object_test_position.filter(|_| marker.playing);
        let at = match reported {
            Some(p) => [p.x, p.y, p.z],
            None => marker.placed,
        };
        let mut changed = false;
        // The level goes through the same meter map every other object's row
        // uses: Studio could not compute it, since the generator is scaled
        // towards the requested level and clamped a little under it.
        if let Some(p) = reported {
            let level = Meter {
                peak_dbfs: p.peak_dbfs,
                rms_dbfs: p.rms_dbfs,
            };
            if live.app.source_levels.get(SOURCE_ID) != Some(&level) {
                live.app.source_levels.insert(SOURCE_ID.to_owned(), level);
                changed = true;
            }
        }
        match live.app.sources.get_mut(SOURCE_ID) {
            Some(source) => {
                if [source.x, source.y, source.z] != at {
                    source.x = at[0];
                    source.y = at[1];
                    source.z = at[2];
                    changed = true;
                }
            }
            None => {
                live.app.sources.insert(
                    SOURCE_ID.to_owned(),
                    SourcePosition {
                        x: at[0],
                        y: at[1],
                        z: at[2],
                        coord_mode: Some("cartesian".to_owned()),
                        // No `name`: the marker's label is translated text, and
                        // the model holds codes. The view names it from its id.
                        ..Default::default()
                    },
                );
                changed = true;
            }
        }
        Tick {
            changed,
            next: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::dispatch::ObjectTestPosition;

    #[test]
    fn the_marker_follows_the_renderer_only_while_it_plays() {
        let state = crate::host::commands::tests::state();
        let mut service = ObjectTestSource::default();
        set_object_test_marker(
            &state,
            ObjectTestMarker {
                shown: true,
                playing: false,
                placed: [0.5, 0.0, -0.5],
            },
        );
        state.inner.lock().unwrap().object_test_position = Some(ObjectTestPosition {
            x: -1.0,
            y: -1.0,
            z: -1.0,
            peak_dbfs: -6.0,
            rms_dbfs: -12.0,
        });

        // Stopped: the handle stays where it was put, and no level is claimed
        // for a source that is making no sound.
        assert!(service.tick(&state, Instant::now()).changed);
        {
            let live = state.inner.lock().unwrap();
            let source = &live.app.sources[SOURCE_ID];
            assert_eq!([source.x, source.y, source.z], [0.5, 0.0, -0.5]);
            assert!(!live.app.source_levels.contains_key(SOURCE_ID));
        }

        // Playing: the scene shows where the source actually is.
        set_object_test_marker(
            &state,
            ObjectTestMarker {
                shown: true,
                playing: true,
                placed: [0.5, 0.0, -0.5],
            },
        );
        assert!(service.tick(&state, Instant::now()).changed);
        {
            let live = state.inner.lock().unwrap();
            let source = &live.app.sources[SOURCE_ID];
            assert_eq!([source.x, source.y, source.z], [-1.0, -1.0, -1.0]);
            assert_eq!(live.app.source_levels[SOURCE_ID].peak_dbfs, -6.0);
        }

        // Settled: a pass that finds nothing new asks for no repaint.
        assert!(!service.tick(&state, Instant::now()).changed);
    }

    #[test]
    fn switching_the_feature_off_takes_the_marker_out_of_the_room() {
        let state = crate::host::commands::tests::state();
        let mut service = ObjectTestSource::default();
        set_object_test_marker(
            &state,
            ObjectTestMarker {
                shown: true,
                ..Default::default()
            },
        );
        service.tick(&state, Instant::now());
        assert!(
            state
                .inner
                .lock()
                .unwrap()
                .app
                .sources
                .contains_key(SOURCE_ID)
        );

        set_object_test_marker(&state, ObjectTestMarker::default());
        assert!(service.tick(&state, Instant::now()).changed);
        assert!(
            !state
                .inner
                .lock()
                .unwrap()
                .app
                .sources
                .contains_key(SOURCE_ID)
        );
        // And it stays gone without asking for a frame every pass.
        assert!(!service.tick(&state, Instant::now()).changed);
    }
}
