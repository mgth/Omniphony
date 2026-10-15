# Measurement sessions, version 1

The companion can discover renderer outputs and play an identified finite sweep
under an exclusive, temporary session. This increment does not acquire microphones,
estimate acoustic delays, apply calibration profiles or save renderer settings.

## Transport and ownership

Use the existing loopback TCP OSC listener, normally 127.0.0.1:9000. Each OSC packet
has the existing four-byte, big-endian length prefix. Send one JSON string to
`/omniphony/control/measurement`; replies use
`/omniphony/state/measurement`. Registration is unnecessary.

The optional `measurement` feature of `omniphony-osc-contract` provides the
shared Rust schema. Requests contain `version: 1`, a positive `request_id`
and a tagged `command`. Reply version and request ID must match. Unexpected
replies, truncated frames or timeouts require closing the connection and
discovering again. Do not blindly retry a signal start.

Example discovery request:

```json
{"version":1,"request_id":1,"command":{"type":"discover"}}
```

Commands:

| Command | Fields | Result |
| --- | --- | --- |
| `discover` | none | Output snapshot and availability |
| `begin` | `expected_revision` | New session ID, confirmed snapshot, lease |
| `start` | `session_id`, increasing `signal_id`, `output_id`, `signal` | Accepted signal and render progress |
| `status` | `session_id` | Current progress; renews lease |
| `stop` | `session_id` | Cancels the current burst; keeps session isolation |
| `end` | `session_id` | Cancels playback and releases the session |

Only the owning connection can start, stop, renew or end its session. Other
control writes are refused while it owns the renderer. Discovery remains
available. Request IDs must increase within the session; completed/cancelled
signal IDs cannot be reused. A second start while a signal is running is refused.

The lease lasts 3000 ms. Clients should poll every 200–1000 ms. Disconnection,
lease expiry, listener shutdown or an observed configuration change releases
ownership. The audio thread independently checks the lease and a signal deadline:
a stuck listener cannot leave an unbounded signal running. Stop/expiry takes effect
at a render-block boundary; already queued host audio and output-delay tails are
not retractable.

## Snapshot identity

Outputs use unique layout names as IDs and include their current output index,
gain, delay, mute and declared frequency range. Renaming changes identity.
Indices must never be reused from a different snapshot. Instance/revision values
are opaque and must be rediscovered after reconnection or configuration changes;
they are not permanent installation identifiers.

The snapshot records the render sample rate, master gain and the 100 ms delay
limit. Its revision includes layout, output settings, relevant renderer epochs
and the host state generation. Asynchronous changes terminate the session at the
control poll; sample rate, width, gain and speaker/topology generation mismatches
also cancel playback in the render path. Discovery reports unsupported conditions
such as headphone mode, a pending layout rebuild or another active test.

Names identify renderer outputs. Confirm the audio device's physical routing
before interpreting them as measured loudspeakers.

## Sweep and render progress

`signal` has `duration_ms`, `peak_dbfs`, `start_hz` and `end_hz`.
Version 1 defines a logarithmic sine sweep with 5 ms raised-cosine fades, prepared
off the audio thread. Bounds:

- Sample rate: 8000–192000 Hz; sweep: 100–5000 ms.
- Injection peak: -60 to -12 dBFS; after current master/output gains: at most -6 dBFS.
- Frequencies increase from at least 20 Hz, stay below 0.45 times the sample rate,
  and remain within the selected output's declared band.
- Muted outputs cannot start a sweep.

Each burst includes 250 ms silence before and after the sweep to flush existing
output delays. `total_frames` and `rendered_frames` include this silence.
The sweep goes directly to the selected output before its existing gain, mute
and delay. It bypasses spatial panning and crossover processing; programme
loudness metadata does not scale it. Other programme/test contributions are
silenced during the session. This signal path must be recorded by future
acquisition code and must not be confused with a measurement through every
programme-processing stage.

Progress distinguishes `queued`, `rendering`, `completed`, `cancelled`
and `expired`. Completion means samples reached the renderer's output stage;
it does not establish a DAC timestamp, sound emission or microphone arrival.
No rendering within the sweep duration plus settling silence plus two seconds
expires the signal. Embedded hosts must keep rendering; the standalone CLI
uses its existing idle feeder during the lease.

The acquisition milestone must add a shared audio/acoustic timing reference,
clock handling and recording metadata. OSC acknowledgement time is unsuitable
for estimating propagation delay.

## Validation without audio hardware

The engine's `measurement_fixture` example opens a real OSC listener and renders
into discarded memory:

```sh
cd omniphony-renderer
cargo run -p orender_engine --example measurement_fixture -- 19003 30
```

Connect the companion to `127.0.0.1:19003`, discover, then sweep one returned
output ID. This exercises both repositories without opening an audio device.
Tests cover TCP ownership, stale configurations, malformed messages, replay,
lease/disconnect handling, stalled rendering, output gain/delay, isolation and
zero allocations during measurement rendering.
