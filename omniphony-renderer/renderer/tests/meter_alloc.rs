//! A meter snapshot is taken on the render thread at the meter cadence: once
//! its lists have grown to the stream, refilling one must not touch the heap.
//! Its own test binary, for the counting allocator.

use renderer::backend_conformance::{CountingAllocator, count_allocations};
use renderer::metering::{AudioMeter, MeterSnapshot};

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

#[test]
fn a_warmed_up_snapshot_is_refilled_without_allocating() {
    let (_, counted) = count_allocations(|| vec![0u8; 16]);
    assert_eq!(counted, 1, "the counting allocator is the global one");

    const OBJECTS: usize = 6;
    const SPEAKERS: usize = 12;
    // A rate past any interval, so every poll is due.
    let mut meter = AudioMeter::new(SPEAKERS, 1.0e9);
    meter.update_channel_count(OBJECTS);
    let objects = vec![0.25f32; OBJECTS];
    let speakers = vec![0.1f32; SPEAKERS * 4];
    let ears = vec![0.2f32; 8];
    // Band energy for some objects only: the listed entries must keep their
    // band lists from one snapshot to the next.
    let bands: Vec<(usize, Vec<f64>)> = (0..OBJECTS)
        .step_by(2)
        .map(|ch| (ch, vec![0.01; 3]))
        .collect();
    let mut snapshot = MeterSnapshot::default();
    let feed_and_poll = |meter: &mut AudioMeter, snapshot: &mut MeterSnapshot| {
        meter.process_objects(&objects, OBJECTS);
        meter.process_object_bands(&bands);
        meter.process_speakers(&speakers, SPEAKERS);
        meter.process_ears(&ears);
        assert!(meter.poll_into(snapshot), "a snapshot is due");
    };
    feed_and_poll(&mut meter, &mut snapshot);
    assert_eq!(snapshot.object_levels.len(), OBJECTS);
    assert_eq!(snapshot.object_band_levels.len(), bands.len());
    assert_eq!(snapshot.speaker_levels.len(), SPEAKERS);
    assert!(snapshot.ear_levels.is_some());

    let (_, allocations) = count_allocations(|| {
        for _ in 0..100 {
            feed_and_poll(&mut meter, &mut snapshot);
        }
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocation(s) in 100 snapshots"
    );
    assert_eq!(snapshot.object_band_levels.len(), bands.len());
    assert!(
        snapshot
            .object_band_levels
            .iter()
            .all(|(_, b)| b.len() == 3),
        "{:?}",
        snapshot.object_band_levels
    );
}
