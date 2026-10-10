//! The reference clock every timestamp of the sync stage is taken on.
//!
//! `CLOCK_MONOTONIC` on Unix: it is what PipeWire's `pw_time.now` and
//! `spa_io_clock.nsec` use, so device and capture timestamps from PipeWire
//! need no conversion, and anything timed here (a pipe reader) lands on the
//! same axis. Its NTP slew (about +11 ppm on the test machine, spike S2)
//! applies to both sides alike and cancels out of the ratio.

/// Now on the reference clock, in seconds.
#[cfg(unix)]
pub fn reference_now_s() -> f64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer; CLOCK_MONOTONIC always exists.
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

/// Now on the reference clock, in seconds (process-relative off Unix until
/// the cpal adapters map their host clocks onto it).
#[cfg(not(unix))]
pub fn reference_now_s() -> f64 {
    use std::sync::OnceLock;
    use std::time::Instant;
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advances() {
        let a = reference_now_s();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = reference_now_s();
        assert!(b - a >= 0.004, "{a} -> {b}");
    }
}
