//! Shared scene state: what the OSC thread writes and the UI thread reads.
//!
//! Positions are kept in the layout frame (x right, y front, z up, normalized
//! to `[-1, 1]`), which is also the frame the renderer speaks on the wire.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct SceneObject {
    pub id: u32,
    pub pos: [f32; 3],
    pub label: String,
    /// Fixed-channel (bed) object as flagged by `/omniphony/object/<id>/meta`.
    pub fixed: bool,
    pub last_seen: Instant,
}

#[derive(Clone, Debug)]
pub struct Speaker {
    pub name: String,
    pub pos: [f32; 3],
    pub spatialize: bool,
}

#[derive(Default)]
pub struct Scene {
    pub objects: BTreeMap<u32, SceneObject>,
    pub speakers: Vec<Speaker>,
    pub layout_name: String,
}

pub type SharedScene = Arc<Mutex<Scene>>;

impl Scene {
    pub fn upsert_position(&mut self, id: u32, pos: [f32; 3], name: Option<&str>) {
        let now = Instant::now();
        match self.objects.get_mut(&id) {
            Some(obj) => {
                obj.pos = pos;
                obj.last_seen = now;
                if let Some(n) = name
                    && obj.label != n
                {
                    obj.label = n.to_owned();
                }
            }
            None => {
                self.objects.insert(
                    id,
                    SceneObject {
                        id,
                        pos,
                        label: name.map(str::to_owned).unwrap_or_else(|| format!("#{id}")),
                        fixed: false,
                        last_seen: now,
                    },
                );
            }
        }
    }

    pub fn upsert_meta(&mut self, id: u32, fixed: bool, label: Option<&str>) {
        let obj = self.objects.entry(id).or_insert_with(|| SceneObject {
            id,
            pos: [0.0; 3],
            label: format!("#{id}"),
            fixed,
            last_seen: Instant::now(),
        });
        obj.fixed = fixed;
        if let Some(l) = label {
            obj.label = l.to_owned();
        }
    }

    pub fn remove(&mut self, id: u32) {
        self.objects.remove(&id);
    }

    /// Drop objects that stopped updating. The real Studio also gets explicit
    /// removals; the timeout is the safety net for a renderer that vanished.
    pub fn prune(&mut self, older_than: std::time::Duration) {
        let now = Instant::now();
        self.objects
            .retain(|_, o| now.duration_since(o.last_seen) < older_than);
    }
}
