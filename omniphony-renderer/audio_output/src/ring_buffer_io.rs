use crossbeam::queue::ArrayQueue;
use std::thread;
use std::time::{Duration, Instant};

pub struct WriteSamplesReport {
    pub pushed_samples: usize,
    pub wait_count: u32,
    pub timed_out: bool,
}

pub fn push_samples_with_backpressure(
    buffer: &ArrayQueue<f32>,
    samples: &[f32],
    max_buffer_fill: usize,
    sleep_ms: u64,
    timeout_waits: u32,
) -> WriteSamplesReport {
    let mut sample_idx = 0usize;
    let mut wait_count = 0u32;

    while sample_idx < samples.len() {
        let buffer_level = buffer.len();
        if buffer_level >= max_buffer_fill {
            wait_count = wait_count.saturating_add(1);
            thread::sleep(Duration::from_millis(sleep_ms));
            if wait_count > timeout_waits {
                return WriteSamplesReport {
                    pushed_samples: sample_idx,
                    wait_count,
                    timed_out: true,
                };
            }
            continue;
        }

        while sample_idx < samples.len() && buffer.len() < max_buffer_fill {
            if buffer.push(samples[sample_idx]).is_ok() {
                sample_idx += 1;
            } else {
                break;
            }
        }
    }

    WriteSamplesReport {
        pushed_samples: sample_idx,
        wait_count,
        timed_out: false,
    }
}

pub struct FlushReport {
    pub timed_out: bool,
    pub stalled: bool,
    pub remaining_samples: usize,
}

pub fn flush_ring_buffer(
    buffer: &ArrayQueue<f32>,
    timeout: Duration,
    poll_interval: Duration,
    stall_timeout: Option<Duration>,
) -> FlushReport {
    let start = Instant::now();
    let mut last_level = buffer.len();
    let mut last_change = start;

    while !buffer.is_empty() {
        if start.elapsed() > timeout {
            let remaining = buffer.len();
            while buffer.pop().is_some() {}
            return FlushReport {
                timed_out: true,
                stalled: false,
                remaining_samples: remaining,
            };
        }

        thread::sleep(poll_interval);
        let current = buffer.len();
        if current < last_level {
            last_level = current;
            last_change = Instant::now();
        } else if let Some(stall_timeout) = stall_timeout {
            if last_change.elapsed() > stall_timeout {
                let remaining = current;
                while buffer.pop().is_some() {}
                return FlushReport {
                    timed_out: false,
                    stalled: true,
                    remaining_samples: remaining,
                };
            }
        }
    }

    FlushReport {
        timed_out: false,
        stalled: false,
        remaining_samples: 0,
    }
}
