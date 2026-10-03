//! The streaming side of the non-uniform convolver runs on the audio thread:
//! it must not touch the heap, on a plain period or on one that blends two
//! kernel sets. Its own test binary, for the counting allocator.

use renderer::backend_conformance::{CountingAllocator, count_allocations};
use renderer::partitioned_conv::nonuniform::{NonUniformKernel, NonUniformPlan};

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

fn noise(len: usize, seed: u32) -> Vec<f32> {
    let mut s = seed;
    (0..len)
        .map(|_| {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (s >> 8) as f32 / (1 << 24) as f32 - 0.5
        })
        .collect()
}

#[test]
fn streaming_a_ladder_does_not_allocate() {
    let plan = NonUniformPlan::new(&[16, 64, 256]);
    let taps = 3000;
    let levels = plan.levels_for(taps);
    assert_eq!(levels, 3, "the kernels reach every level");
    let (inputs, outputs) = (3, 2);
    // Two kernel sets, `[set][input][output]`, to blend between.
    let sets: Vec<Vec<Vec<NonUniformKernel>>> = (0..2)
        .map(|set| {
            (0..inputs)
                .map(|i| {
                    (0..outputs)
                        .map(|o| {
                            let seed = (set * 100 + i * 10 + o) as u32;
                            plan.partition(&noise(taps, seed), levels)
                        })
                        .collect()
                })
                .collect()
        })
        .collect();
    let head = plan.head();
    let block = head.block();
    let mut histories: Vec<_> = (0..inputs)
        .map(|_| head.make_input(plan.partitions_for(0, levels, taps)))
        .collect();
    let mut tails = plan.make_tails(levels, taps, inputs, outputs);
    let mut scratch = head.make_scratch();
    let mut out = vec![vec![0.0f32; block]; outputs];
    let signal = noise(block * 16, 7);

    let (_, counted) = count_allocations(|| vec![0u8; 16]);
    assert_eq!(counted, 1, "the counting allocator is the global one");
    let mut energy = 0.0f32;
    let ((), allocations) = count_allocations(|| {
        for head_block in 0..600 {
            for history in &mut histories {
                for &x in &signal[head_block % 16 * block..][..block] {
                    if history.push(x) {
                        head.analyze(history);
                    }
                }
            }
            tails.feed(histories.iter().map(|h| h.last_block()));
            for (o, out) in out.iter_mut().enumerate() {
                scratch.clear();
                for (i, history) in histories.iter().enumerate() {
                    head.accumulate(history, sets[0][i][o].head(), &mut scratch);
                }
                head.finish(&mut scratch, out);
            }
            // Every other stretch of 64 head blocks blends the two sets.
            let blend = head_block / 64 % 2 == 1;
            for segment in 0..tails.segments() {
                tails.run(
                    segment,
                    blend,
                    |pass, i, o| Some(sets[pass][i][o].tail(segment)),
                    &mut out,
                );
            }
            energy += out.iter().flatten().map(|v| v * v).sum::<f32>();
        }
    });
    assert!(energy > 0.0 && energy.is_finite());
    assert_eq!(
        allocations, 0,
        "the streaming path allocated {allocations} time(s)"
    );
}
