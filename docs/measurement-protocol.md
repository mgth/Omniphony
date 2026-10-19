# Measurement sessions, version 5

The companion can discover renderer outputs and play an identified finite sweep
under an exclusive, temporary session. This increment does not acquire microphones,
estimate acoustic delays or save renderer settings. It can apply absolute gain/delay targets after a revision check.

## Transport and ownership

Use the existing loopback TCP OSC listener, normally 127.0.0.1:9000. Each OSC packet
has the existing four-byte, big-endian length prefix. Send one JSON string to
`/omniphony/control/measurement`; replies use
`/omniphony/state/measurement`. Registration is unnecessary.

The optional `measurement` feature of `omniphony-osc-contract` provides the
shared Rust schema. Requests contain `version: 5`, a positive `request_id`
and a tagged `command`. Reply version and request ID must match. Renderers
also answer `version: 4` requests in the version-4 shape (see
[Output positions](#output-positions-version-5)); any other version is refused
with `unsupported_version` in a version-5 reply. Unexpected
replies, truncated frames or timeouts require closing the connection and
discovering again. Do not blindly retry a signal start.

Example discovery request:

```json
{"version":5,"request_id":1,"command":{"type":"discover"}}
```

Commands:

| Command | Fields | Result |
| --- | --- | --- |
| `discover` | none | Output snapshot and availability |
| `begin` | `expected_revision` | New session ID, confirmed snapshot, lease |
| `start` | `session_id`, increasing `signal_id`, `output_id`, optional `reference_output_id`, optional `sequence_outputs`, `signal` | Accepted signal and render progress |
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
gain, delay, mute, declared frequency range and, from version 5, their layout
position. Renaming changes identity.
Indices must never be reused from a different snapshot. Instance/revision values
are opaque and must be rediscovered after reconnection or configuration changes;
they are not permanent installation identifiers.

The snapshot records the render sample rate, master gain and the 250 ms delay
limit. Its revision includes layout, output settings, relevant renderer epochs
and the host's measurement configuration. Normal output startup, device-list
refreshes and held-latency telemetry do not change that configuration: beginning
an idle measurement wakes the audio stream without invalidating its own session.
Output setting changes and output faults still terminate the session. Hosts that
do not distinguish settings from telemetry retain conservative invalidation on
any host state change. Asynchronous changes terminate the session at the
control poll; sample rate, width, gain and speaker/topology generation mismatches
also cancel playback in the render path. Discovery reports unsupported conditions
such as headphone mode, a pending layout rebuild or another active test. A speaker
count that differs from the running output width is rejected before acquisition;
save the layout in Studio and restart the renderer to reopen it at the new width.

Names identify renderer outputs. Confirm the audio device's physical routing
before interpreting them as measured loudspeakers.

## Sweep and render progress

`signal` has `duration_ms`, `peak_dbfs`, `start_hz` and `end_hz`.
Versions 2 and 3 define a logarithmic sine sweep with 5 ms raised-cosine fades, prepared
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

## Repeated acoustic reference

With a non-null `reference_output_id`, Start schedules three identical segments:
reference, target, reference. Segment start offsets are exactly `0`, `N`, and
`2N` renderer frames, where `N = Sweep::frames(sample_rate)` includes the silence.
Progress echoes the reference ID and reports `3N` total frames. With null/omitted
reference ID, Start still plays one target segment. The live protocol versions are 4 and 5;
a version-5 client talking to an older renderer must fall back to version 4.
The sweep waveform is unchanged from version 2.

Both outputs are checked for mute, frequency and level limits before playback.
The prepared mono buffer is shared by all segments. Routing is resolved once
per block/segment boundary, with no allocation in the audio path. The signal
deadline covers all three segments plus two seconds; the session still requires
keepalives every 200–1000 ms.

The companion records continuously and correlates the two repeated references.
Their spacing estimates the capture/render clock ratio; the middle peak's
difference from their midpoint retains the target's relative propagation and
existing output delay. Use the same fixed reference output and microphone
position across compared takes. Reference and target may be the same output
for a timing sanity check.

These are correlation-peak diagnostics, not a validated direct-arrival estimator.
Nonlinear clock drift, hidden resampling, acoustic multipath and actual hardware
repeatability still require validation. OSC acknowledgement time remains
unsuitable for estimating propagation delay.

Calibration changes are developed against `dev/calibration`. This experimental
contract is not a change to the main renderer branch.

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

## Continuous shared-reference sequences

A nonempty `sequence_outputs` array in Start contains ordered, unique target IDs.
`output_id` must equal its first entry, and `reference_output_id` is required.
Playback is `reference, target[0], reference, target[1], ..., reference`: exactly
`2 * targets + 1` equal-length segments. Progress echoes the full ordered list
and reports the total frame count for the entire sequence. Empty/omitted lists
retain single-target behavior. Protocol 3 makes this identity explicit.

All target and reference mute, frequency and post-gain level checks happen before
installing any signal. Sequences contain at most 32 targets and last at most
120 seconds, including settling silence. The normal 3-second lease still requires
keepalives and stops abandoned playback independently of the UI. The sequence's
wall-clock deadline includes its complete duration plus two seconds.

The control thread builds a bounded list of output indices and one reusable mono
sweep. The renderer changes the selected index only at segment boundaries, with
no audio-thread allocation. Continuous acquisition can retain overlapping
reference–target–reference windows for analysis: a shared reference must use the
same recorded samples, not a replay or a recording from another session.

## Temporary gain/delay settings

Version 4 adds `set_outputs`, with `expected_revision` and a nonempty `settings`
array of `{output_id, gain_db, delay_ms}`. Values are absolute, limited to
-60..24 dB and 0..250 ms. Duplicate/unknown outputs and nonfinite values reject
all changes. An active measurement session blocks application. The current
revision is checked while holding the live-state writer lock; all selected outputs
are published together, then a fresh snapshot is returned. Repeated application
of the same values does not add offsets. Unselected outputs are preserved.

The command does not save configuration to disk. Restoration uses the same
command with the last confirmed revision and the original absolute values.
A missing acknowledgement leaves application uncertain: discover and inspect
state rather than blindly replaying or claiming success. Independent topology or
host reconfiguration must still be excluded during calibration; a subsequent
snapshot/reacquisition detects a changed configuration.

Saved protocol-2/3 sweep recordings retain their waveform and can be analyzed
offline; live clients and renderers must both use version 4.

The 250 ms limit is backed by the physical speaker delay rings, allocated when
constructing the speaker stage. It fits the existing 250 ms sweep padding, so
protocol version 4 and the saved waveform definition remain unchanged. Clients
must use the advertised `max_delay_ms` for proposals; older version-4 builds
advertise 100 ms. Long measured route latency is not itself invalid evidence.
Delaying earlier outputs to match a slow device also adds playback latency and
may require audio/video synchronization adjustments.


## Optional v4 physical-level noise extension

`start_noise` takes the session ID, increasing signal ID, physical output ID and
`signal: {duration_seconds, peak_dbfs}`. It prepares periodic 500–2000 Hz pink
noise (1/f power), limited to 5–60 seconds and -60..-12 dBFS peak. Preparation,
FFT and reclamation stay off the audio thread; one second of PCM is reused.
Five-millisecond onset/offset fades do not interrupt the intervening noise.
The output must cover the complete band and remain under the existing -6 dBFS
ceiling after master/output gains. Subwoofer matching is outside this extension.

Start and subsequent status/stop return `noise_session`, with the unchanged
session ID/snapshot/lease and a `NoiseProgress` object containing the exact noise
specification, output and signal IDs, state, submitted frames and total frames.
Noise and sweeps are exclusive. The same owner, monotonic request/signal IDs,
three-second lease, finite audio-thread deadline, Stop/End, disconnect and
configuration-change rules apply. Existing digital settings are never changed.

This is an additive v4 command/reply pair: existing sweep clients and saved
sweep metadata keep their wire representation. Older v4 renderers reject the
unknown command before playback. Clients must report that a compatible build is
needed and must never fall back to the legacy unbounded speaker-test command.
The protocol makes no SPL or acoustic level claim; the companion acquires and
compares microphone power separately.

## Output positions, version 5

Version 5 adds three optional fields to each snapshot output, for display only:

| Field | Meaning |
| --- | --- |
| `azimuth_deg` | 0° front, negative to the left, positive to the right, ±180° behind |
| `elevation_deg` | 0° at ear level, positive up |
| `distance_m` | Distance from the listening position in metres |

They are the active layout's declared geometry, computed as Studio shows it:
the speaker's normalized coordinates through the room warp the topology pans
in, at the layout's room scale (`radius_m`, half the room width). A polar
speaker without a stated distance therefore sits on the layout's unit radius.
They are not measured or acoustic positions, and they never qualify a delay or
level correction. A field is omitted when the renderer cannot place the output.
Positions are part of the layout, so a moved speaker already changes the
snapshot revision. The snapshot is built on the control thread when a client
asks for it (and at each session poll); the audio path never computes it.

Version-4 clients parse outputs strictly and would refuse these fields. A
renderer therefore answers a `version: 4` request with a `version: 4` reply whose
snapshots omit them; commands and every other field are unchanged. A version-5
client receives positions, and treats every field as optional: version-4 data,
saved snapshots and hosts that cannot place an output leave them absent. An
older renderer answers a version-5 request with `unsupported_version` in a
version-4 reply; the client may then continue in version 4 without positions.
