//! Server-side cache of the per-band speaker gain table.
//!
//! Building the per-band table (N band topologies sampled over the grid) is the
//! expensive part, so the **full raw table** (all speakers) is built once per
//! topology and cached. Each subscriber only ever needs **one** speaker, so the
//! wire bytes are serialized per speaker on demand (cheap zlib of one column) —
//! `speaker_count`× less data than shipping the whole table. The cache is
//! invalidated on topology rebuild and lazily rebuilt on next access (returns
//! `None` when no precomputed table is available).

use std::sync::{Arc, RwLock};

use renderer::band_gaintable::BandGaintableFull;
use runtime_control::context::RuntimeControlContext;
use runtime_control::osc::gaintable_version;

pub(crate) struct GaintableCache {
    inner: RwLock<Option<Arc<BandGaintableFull>>>,
}

impl GaintableCache {
    pub(crate) fn new() -> Self {
        Self {
            inner: RwLock::new(None),
        }
    }

    /// Drop the cached table (call on topology rebuild). The next [`ensure`]
    /// rebuilds from the now-current topology.
    pub(crate) fn invalidate(&self) {
        *self.inner.write().unwrap() = None;
    }

    /// Cached full table, building + caching on a miss. `None` when the active
    /// backend has no precomputed table.
    fn ensure(&self, ctx: &RuntimeControlContext) -> Option<Arc<BandGaintableFull>> {
        if let Some(cached) = self.inner.read().unwrap().clone() {
            return Some(cached);
        }
        let full = ctx.renderer.build_band_gaintable_full().ok()?;
        let arc = Arc::new(full);
        *self.inner.write().unwrap() = Some(Arc::clone(&arc));
        Some(arc)
    }

    /// Serialize one speaker's per-band field for transfer: `(version, bytes)`.
    /// `None` when there's no table.
    ///
    /// The negative sentinels select all-speaker derived fields instead of a
    /// single speaker's slice — same container, same size, so the chunking
    /// path is identical: [`renderer::band_gaintable::GLOBAL_ENERGY_INDEX`]
    /// (total energy), [`renderer::band_gaintable::GAIN_DISCONTINUITY_INDEX`]
    /// (normalised gain-vector jump between neighbouring cells) and
    /// [`renderer::band_gaintable::CENTROID_JUMP_INDEX`] (energy-centroid room
    /// distance to the worst neighbour).
    pub(crate) fn bytes_for_target(
        &self,
        ctx: &RuntimeControlContext,
        target: i64,
    ) -> Option<(u32, Arc<Vec<u8>>)> {
        let full = self.ensure(ctx)?;
        let bytes = match target {
            renderer::band_gaintable::GLOBAL_ENERGY_INDEX => full.serialize_energy(),
            renderer::band_gaintable::GAIN_DISCONTINUITY_INDEX => {
                full.serialize_gain_discontinuity()
            }
            renderer::band_gaintable::CENTROID_JUMP_INDEX => full.serialize_centroid_jump(),
            _ => full.serialize_for_speaker(target.max(0) as usize),
        };
        let version = gaintable_version(&bytes);
        Some((version, Arc::new(bytes)))
    }
}
