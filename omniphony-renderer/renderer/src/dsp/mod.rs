//! Small DSP building blocks shared by the render paths (speaker stage,
//! crossover, binaural stage, test signals) and by the engine's channel-plan
//! generators, so each formula exists once.

pub mod db;
pub mod iir;

/// Put the calling thread's FPU in flush-to-zero / denormals-are-zero mode,
/// once per thread (issue #154).
///
/// Every recursive DSP path in the renderer (FDN delay lines and damping,
/// reflection-tap smoothing, air-absorption one-poles, biquad states) decays
/// exponentially toward zero after input stops; without FTZ those tails enter
/// denormal range, where each multiply can cost 10–100× on x86 — a CPU spike
/// exactly when the stream goes silent. Flushing to zero is the standard
/// audio-DSP trade: values below ~1e-38 are ~−760 dBFS, far beyond audibility.
///
/// This claims the FP environment of the host's thread (mpv's decode thread,
/// the CLI engine), which is deliberate: that thread runs our DSP, and FTZ is
/// the conventional processing mode for realtime audio. `render_frame` calls
/// it at its entry; a host thread that runs other renderer DSP first may call
/// it itself. On architectures without an FTZ switch here (armv7, unknown
/// targets) it is a no-op — correct, just without the protection, which is
/// why the recursive stages that decay on silence keep their own per-sample
/// flush as well.
#[inline]
pub fn ensure_denormals_flushed() {
    use std::cell::Cell;
    thread_local! {
        static CLAIMED: Cell<bool> = const { Cell::new(false) };
    }
    CLAIMED.with(|claimed| {
        if claimed.get() {
            return;
        }
        claimed.set(true);
        #[cfg(target_arch = "x86_64")]
        unsafe {
            // MXCSR bits: FTZ = 15, DAZ = 6 (DAZ exists on every x86-64 CPU
            // this crate targets). Inline asm instead of the deprecated
            // `_mm_setcsr` intrinsics: the write is opaque to LLVM, which is
            // the point — the changed FP mode must not be reasoned away.
            let mut mxcsr: u32 = 0;
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut mxcsr, options(nostack));
            mxcsr |= (1 << 15) | (1 << 6);
            std::arch::asm!("ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
        }
        #[cfg(target_arch = "aarch64")]
        unsafe {
            // FPCR.FZ (bit 24): flush-to-zero for f32/f64 (Apple Silicon
            // builds). Read-modify-write keeps the rounding mode intact.
            let mut fpcr: u64;
            std::arch::asm!("mrs {}, fpcr", out(reg) fpcr);
            fpcr |= 1 << 24;
            std::arch::asm!("msr fpcr, {}", in(reg) fpcr);
        }
    });
}
