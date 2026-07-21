//! Format-agnostic mapping from decoded spatial metadata to the renderer's
//! per-channel events. Shared by the engine and the CLI host.

use crate::events::{Configuration, Event};
use crate::osc::ObjectMeta;
use bridge_api::{RChannelGain, RChannelLabel, RCoordinateFormat};
use renderer::spatial_renderer::SpatialChannelEvent;

use std::collections::HashMap;

/// Wrap an azimuth in degrees into `[-180, 180]`.
pub fn normalize_azimuth_deg(mut azimuth_deg: f32) -> f32 {
    while azimuth_deg < -180.0 {
        azimuth_deg += 360.0;
    }
    while azimuth_deg > 180.0 {
        azimuth_deg -= 360.0;
    }
    azimuth_deg
}

/// Raw, unconverted position `[x, y, z]` exactly as the event carries it (no
/// coordinate-format conversion). Used for OSC object broadcast, where the
/// coordinate format is sent alongside the values.
pub fn event_pos_raw(event: &Event) -> Option<[f64; 3]> {
    let p = event.pos()?;
    if p.len() < 3 {
        return None;
    }
    Some([p[0], p[1], p[2]])
}

/// Legacy 0-9 bed ids of the fixed channels, in PCM order, stopping at the
/// first object channel. EXPORT-order helper only (file-output bed
/// conformance keeps the fixed 10-slot layout); rendering uses the channel
/// plan instead.
pub fn derive_bed_indices(channel_labels: &[RChannelLabel]) -> Vec<usize> {
    channel_labels
        .iter()
        .take_while(|l| **l != RChannelLabel::Object)
        .map(|l| renderer::speaker_layout::legacy_bed_id(*l).unwrap_or(usize::MAX))
        .collect()
}

/// Build the OSC broadcast objects for the dynamic objects of one metadata
/// payload (fixed channels are displayed separately, from the channel plan —
/// see `virtual_bed::build_fixed_channel_objects`). Objects report their raw
/// event position in the bridge's coordinate format, named from the
/// accumulated `object_names` map (falling back to `Obj_<id>`).
pub fn build_object_metas(
    conf: &Configuration,
    coordinate_format: RCoordinateFormat,
    object_names: &HashMap<u32, String>,
) -> Vec<ObjectMeta> {
    let fallback_coord_mode = || match coordinate_format {
        RCoordinateFormat::Cartesian => "cartesian".to_string(),
        RCoordinateFormat::Polar => "polar".to_string(),
    };
    conf.events
        .iter()
        .filter_map(|event| {
            let id = event.id()?;
            let [x, y, z] = event_pos_raw(event).unwrap_or([0.0; 3]);
            Some(ObjectMeta {
                name: object_names
                    .get(&id)
                    .cloned()
                    .unwrap_or_else(|| format!("Obj_{id}")),
                x: x as f32,
                y: y as f32,
                z: z as f32,
                coord_mode: fallback_coord_mode(),
                direct_speaker_index: None,
                gain: event.gain_db().map_or(-128, |g| g as i32),
                priority: 0.0,
                size: event
                    .size()
                    .map(|s| [s[0] as f32, s[1] as f32, s[2] as f32])
                    .unwrap_or([0.0, 0.0, 0.0]),
                fixed: false,
                label: String::new(),
            })
        })
        .collect()
}

/// Resolve an event's position to ADM Cartesian `[x, y, z]`, converting from the
/// bridge's declared coordinate format. Returns `None` when the event carries no
/// position (e.g. a bed channel or a gain-only update).
pub fn event_pos_as_adm_cartesian(
    coordinate_format: RCoordinateFormat,
    event: &Event,
) -> Option<[f64; 3]> {
    let p = event.pos()?;
    if p.len() < 3 {
        return None;
    }

    match coordinate_format {
        RCoordinateFormat::Cartesian => Some([p[0], p[1], p[2]]),
        RCoordinateFormat::Polar => {
            let az = normalize_azimuth_deg(p[0] as f32);
            let el = (p[1] as f32).clamp(-90.0, 90.0);
            let dist = (p[2] as f32).max(0.0);
            let (x, y, z) = renderer::spatial_vbap::spherical_to_adm(az, el, dist);
            Some([x as f64, y as f64, z as f64])
        }
    }
}

/// Build the renderer's per-channel events from one metadata payload and append
/// them to `out`.
///
/// Object events map to their PCM channel through the cached
/// `object_channels` declaration (events for undeclared ids are skipped);
/// `channel_gains` yields one gain/ramp-only event per listed fixed channel.
pub fn build_spatial_channel_events(
    conf: &Configuration,
    coordinate_format: RCoordinateFormat,
    object_channels: &[(u32, usize)],
    channel_gains: &[RChannelGain],
    sample_pos: u64,
    ramp_duration: u32,
    out: &mut Vec<SpatialChannelEvent>,
) {
    for gain in channel_gains {
        out.push(SpatialChannelEvent {
            channel_idx: gain.channel as usize,
            is_bed: true,
            gain_db: Some(gain.gain_db),
            ramp_length: Some(ramp_duration),
            size: None,
            position: None,
            sample_pos: Some(sample_pos),
        });
    }

    for event in &conf.events {
        let Some(object_id) = event.id() else {
            continue;
        };
        let Some(&(_, channel_idx)) = object_channels.iter().find(|(id, _)| *id == object_id)
        else {
            continue;
        };
        out.push(SpatialChannelEvent {
            channel_idx,
            is_bed: false,
            gain_db: event.gain_db(),
            ramp_length: event.ramp_length(),
            size: event
                .size()
                .map(|s| [s[0] as f32, s[1] as f32, s[2] as f32]),
            position: event_pos_as_adm_cartesian(coordinate_format, event),
            sample_pos: event.sample_pos(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bridge_api::RChannelLabel::*;

    fn labels_5_1_2_plus_objects() -> Vec<RChannelLabel> {
        vec![L, R, C, LFE, Ls, Rs, Object, Object]
    }

    #[test]
    fn derive_bed_indices_maps_labels_and_stops_at_objects() {
        assert_eq!(
            derive_bed_indices(&labels_5_1_2_plus_objects()),
            vec![0, 1, 2, 3, 4, 5]
        );
        // A fixed label outside the legacy scheme keeps its slot unroutable.
        assert_eq!(derive_bed_indices(&[L, Tbl, R]), vec![0, usize::MAX, 1]);
    }

    #[test]
    fn channel_events_route_objects_through_the_declaration() {
        let mut event = Event::with_id(11);
        event.set_pos([0.5, 0.5, 0.0]);
        let conf = Configuration::new(vec![event, Event::with_id(99)]);
        let object_channels = [(11u32, 6usize)];
        let gains = [bridge_api::RChannelGain {
            channel: 2,
            gain_db: -3,
        }];

        let mut out = Vec::new();
        build_spatial_channel_events(
            &conf,
            RCoordinateFormat::Cartesian,
            &object_channels,
            &gains,
            480,
            32,
            &mut out,
        );

        // One bed-gain event + one object event; the undeclared id 99 is skipped.
        assert_eq!(out.len(), 2);
        assert!(out[0].is_bed);
        assert_eq!(out[0].channel_idx, 2);
        assert_eq!(out[0].gain_db, Some(-3));
        assert_eq!(out[0].ramp_length, Some(32));
        assert_eq!(out[0].sample_pos, Some(480));
        assert!(!out[1].is_bed);
        assert_eq!(out[1].channel_idx, 6);
    }

    #[test]
    fn object_metas_report_named_objects_with_raw_positions() {
        let mut event = Event::with_id(10);
        event.set_pos([0.1, 0.2, 0.3]);
        event.set_gain_db(-3);
        let conf = Configuration::new(vec![event]);
        let names = HashMap::from([(10, "Music".to_string())]);

        let metas = build_object_metas(&conf, RCoordinateFormat::Cartesian, &names);

        assert_eq!(metas.len(), 1);
        assert_eq!(metas[0].name, "Music");
        assert_eq!(metas[0].direct_speaker_index, None);
        assert_eq!(metas[0].gain, -3);
        assert!((metas[0].x - 0.1).abs() < 1e-6);
    }

    #[test]
    fn unnamed_objects_fall_back_to_their_id() {
        let conf = Configuration::new(vec![Event::with_id(12)]);
        let metas = build_object_metas(&conf, RCoordinateFormat::Cartesian, &HashMap::new());
        assert_eq!(metas[0].name, "Obj_12");
    }
}
