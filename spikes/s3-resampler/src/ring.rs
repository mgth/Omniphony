//! Minimal interleaved frame ring (single-threaded stand-in for the SPSC ring
//! of Phase 2: same bulk two-segment memcpy on both sides, no atomics).

pub struct Ring {
    buf: Vec<f32>,
    ch: usize,
    cap: usize,
    read: u64,
    write: u64,
}

impl Ring {
    pub fn new(ch: usize, cap_frames: usize) -> Self {
        Ring {
            buf: vec![0.0; ch * cap_frames],
            ch,
            cap: cap_frames,
            read: 0,
            write: 0,
        }
    }

    pub fn available(&self) -> usize {
        (self.write - self.read) as usize
    }

    pub fn free(&self) -> usize {
        self.cap - self.available()
    }

    pub fn write(&mut self, src: &[f32]) {
        let frames = src.len() / self.ch;
        assert!(frames <= self.free());
        let start = (self.write % self.cap as u64) as usize;
        let first = frames.min(self.cap - start);
        let c = self.ch;
        self.buf[start * c..(start + first) * c].copy_from_slice(&src[..first * c]);
        self.buf[..(frames - first) * c].copy_from_slice(&src[first * c..]);
        self.write += frames as u64;
    }

    /// Read exactly `dst.len() / ch` frames. Returns false (and reads
    /// nothing) on underrun.
    pub fn read_into(&mut self, dst: &mut [f32]) -> bool {
        let frames = dst.len() / self.ch;
        if frames > self.available() {
            return false;
        }
        let start = (self.read % self.cap as u64) as usize;
        let first = frames.min(self.cap - start);
        let c = self.ch;
        dst[..first * c].copy_from_slice(&self.buf[start * c..(start + first) * c]);
        dst[first * c..].copy_from_slice(&self.buf[..(frames - first) * c]);
        self.read += frames as u64;
        true
    }
}
