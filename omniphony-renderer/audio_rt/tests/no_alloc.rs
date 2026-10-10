//! The steady state allocates nothing: a counting global allocator watches a
//! realistic callback loop (ring push on one side, prepare/read/render/skip on
//! the other) after construction.

use audio_rt::{Design, DriftResampler, frame_ring};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn the_callback_loop_does_not_allocate() {
    const CH: usize = 24;
    let mut rs = DriftResampler::new(CH, &Design::DRIFT_48K, 4096, 1.01);
    let (mut tx, mut rx) = frame_ring(CH, 1 << 15);
    let chunk = vec![0.1f32; 960 * CH];
    let mut out = vec![0.0f32; 4096 * CH];
    let sizes = [1024usize, 256, 4096, 64, 1000];
    let ratios = [1.0, 1.0004, 0.9993, 1.002];

    let before = ALLOCS.load(Ordering::Relaxed);
    for i in 0..400 {
        while tx.free() >= 960 && rx.available() < 12_000 {
            tx.push(&chunk);
        }
        if i % 97 == 50 {
            let discard = rs.skip(1500.25);
            rx.discard(discard);
        }
        let n = sizes[i % sizes.len()];
        rs.prepare(n, ratios[i % ratios.len()]);
        if rx.read_exact(rs.input_slot()) {
            rs.render(&mut out);
        }
    }
    let after = ALLOCS.load(Ordering::Relaxed);
    assert_eq!(
        after - before,
        0,
        "{} allocations in the steady state",
        after - before
    );
}
