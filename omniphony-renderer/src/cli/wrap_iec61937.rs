//! `orender wrap-iec61937`: write an IAMF file as the IEC 61937 burst stream
//! of the private IAMF type (`docs/iamf-transport-plan.md`).
//!
//! A test tool: it stands in for the sender, so the carrier path can be fed
//! and compared with the raw one. The output is the carrier's bytes, 16-bit
//! little-endian words, one burst per temporal unit on its period.

use anyhow::{Context, Result};
use spdif::iamf::{Carrier, IamfSequence, write_bursts};

use super::command::WrapIec61937Args;

pub fn cmd_wrap_iec61937(args: &WrapIec61937Args) -> Result<()> {
    let stream = std::fs::read(&args.input)
        .with_context(|| format!("cannot read {}", args.input.display()))?;
    let sequence = IamfSequence::parse(&stream)
        .with_context(|| format!("{} is not a carriable IAMF stream", args.input.display()))?;
    let carrier = Carrier {
        rate_hz: args.carrier_rate,
        channels: args.channels,
    };
    let period = carrier.period_bytes(sequence.timing, args.sample_rate)?;
    // A copy of the descriptors about once per `copy_interval_ms`.
    let unit_ms =
        1000.0 * period as f64 / (carrier.frame_bytes() as f64 * f64::from(carrier.rate_hz));
    let copy_every = match args.copy_interval_ms {
        0 => 0,
        interval => ((f64::from(interval) / unit_ms).round() as usize).max(1),
    };
    let mut carried = Vec::with_capacity(sequence.units.len() * period);
    write_bursts(&mut carried, &sequence, period, copy_every)?;
    std::fs::write(&args.output, &carried)
        .with_context(|| format!("cannot write {}", args.output.display()))?;
    log::info!(
        "{} unit(s) of {unit_ms:.3} ms, {period}-byte period on {} ch at {} Hz ({:.0} kbit/s), descriptors ({} bytes) repeated every {copy_every} unit(s): {}",
        sequence.units.len(),
        carrier.channels,
        carrier.rate_hz,
        carrier.frame_bytes() as f64 * f64::from(carrier.rate_hz) * 8.0 / 1000.0,
        sequence.descriptors.len(),
        args.output.display()
    );
    Ok(())
}
