//! Frame and process statistics for the gates: frame rate, OSC packet rate,
//! resident memory and CPU share of the whole process.

use std::time::{Duration, Instant};

pub struct FrameStats {
    last_frame: Instant,
    window_start: Instant,
    frames_in_window: u32,
    pub fps: f32,
    /// Exponential moving average of the frame interval, milliseconds.
    pub frame_ms: f32,
}

impl FrameStats {
    pub fn new() -> Self {
        let now = Instant::now();
        Self {
            last_frame: now,
            window_start: now,
            frames_in_window: 0,
            fps: 0.0,
            frame_ms: 0.0,
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.last_frame).as_secs_f32() * 1000.0;
        self.last_frame = now;
        self.frame_ms = if self.frame_ms == 0.0 {
            dt
        } else {
            0.9 * self.frame_ms + 0.1 * dt
        };
        self.frames_in_window += 1;
        let window = now.duration_since(self.window_start);
        if window >= Duration::from_secs(1) {
            self.fps = self.frames_in_window as f32 / window.as_secs_f32();
            self.frames_in_window = 0;
            self.window_start = now;
        }
    }
}

pub struct ProcStats {
    last_sample: Instant,
    last_ticks: Option<u64>,
    last_packets: u64,
    pub cpu_percent: f32,
    pub rss_mb: f32,
    pub packets_per_s: f32,
}

impl ProcStats {
    pub fn new() -> Self {
        let mut s = Self {
            last_sample: Instant::now(),
            last_ticks: cpu_ticks(),
            last_packets: 0,
            cpu_percent: 0.0,
            rss_mb: 0.0,
            packets_per_s: 0.0,
        };
        s.rss_mb = rss_mb();
        s
    }

    /// Re-sample at most once per second. `packets_total` is the OSC packet
    /// counter, differentiated here into a rate.
    pub fn sample(&mut self, packets_total: u64) {
        let elapsed = self.last_sample.elapsed();
        if elapsed < Duration::from_secs(1) {
            return;
        }
        let secs = elapsed.as_secs_f32();
        if let (Some(prev), Some(now)) = (self.last_ticks, cpu_ticks()) {
            // Linux CLK_TCK is 100 on every mainstream distribution; the
            // Studio does not need libc just to confirm it.
            self.cpu_percent = (now.saturating_sub(prev)) as f32 / 100.0 / secs * 100.0;
            self.last_ticks = Some(now);
        }
        self.packets_per_s = (packets_total.saturating_sub(self.last_packets)) as f32 / secs;
        self.last_packets = packets_total;
        self.rss_mb = rss_mb();
        self.last_sample = Instant::now();
    }
}

pub fn rss_mb() -> f32 {
    memory_stats::memory_stats()
        .map(|m| m.physical_mem as f32 / 1_000_000.0)
        .unwrap_or(0.0)
}

/// utime + stime of this process in clock ticks (Linux only).
#[cfg(target_os = "linux")]
fn cpu_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    // Fields after the parenthesised command name; utime/stime are the
    // 14th/15th fields of the line, i.e. index 11/12 after `state`.
    let rest = &stat[stat.rfind(')')? + 2..];
    let mut it = rest.split_ascii_whitespace();
    let utime: u64 = it.nth(11)?.parse().ok()?;
    let stime: u64 = it.next()?.parse().ok()?;
    Some(utime + stime)
}

#[cfg(not(target_os = "linux"))]
fn cpu_ticks() -> Option<u64> {
    None
}
