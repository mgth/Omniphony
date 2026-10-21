# Measurement sessions, version 8

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

## Level noise without the output gain, version 6

Version 6 adds one optional field to the `start_noise` signal:
`bypass_output_gain: true` plays the noise as if the output's own gain (its level
correction) were 0 dB. The master gain still applies, and so do the output's delay
and mute. The renderer divides the prepared noise by that gain on the control
thread, so the audio path is unchanged; a change of the output's gain changes the
speaker generation and cancels the noise as before. The -6 dBFS ceiling then
counts the master gain only, and an output whose gain is below -60 dB is refused
with `level_limit`.

A client matching physical volume controls uses it so the speakers play at the
same level whatever their trims; playing with the gain shows the corrected result.
The field is omitted when false, so a request without it keeps the version-4 wire
form. An older renderer refuses the field (and a version-6 request with
`unsupported_version`); a client that negotiated an older version can only play
the noise through the output's gain.

## Switching to speaker output, version 7

A renderer playing to headphones (binaural output) reports the snapshot
`unavailable_reason` "measurement requires speaker output mode" (the contract's
`SPEAKER_OUTPUT_REQUIRED`): the microphone cannot measure speakers that are not
playing. This can happen without the user choosing it, for instance when a renderer
resumes from standby and adopts the live state another host handed back.

Version 7 adds the command `use_speaker_output: {}`, accepted outside a session
only (`busy` otherwise). It sets the live output mode to the speakers through the
same option as the Studio's output mode, tells the other clients, and replies with
the new snapshot. If a BRIR set's layout was in use, the snapshot may then say to
wait for the speaker layout rebuild, which the renderer runs on its own. Like
`set_outputs`, it is live only: the saved configuration changes only through the
Studio's Save. An older renderer refuses the unknown command; a client offers it
only on a version-7 connection.

## Room correction filters, version 8

Version 8 lets a client measure the speakers without their room correction
filters, play the low band through bass management as programme audio does,
and hand the renderer the filters it designed. The renderer applies them
(`docs/output-correction.md`); it does not design them. Every field and
command below is refused in a request of an older version
(`invalid_request`), and every reply to an older version leaves them out, so
a version-7 client sees the version-7 shapes.

### Uploading a filter

A filter is an impulse response at the rate it was designed at: the
renderer's own, or any rate from 32 to 384 kHz (resampled when it is loaded),
at most 1.5 s long at that rate, finite and not silent. It travels as
`upload_filter` chunks, because a measurement request is one JSON string of
at most 64 KiB (`MAX_PACKET_BYTES`; the stream transport itself carries 1 MiB
packets):

| Field | Meaning |
| --- | --- |
| `upload_id` | Omitted in the first chunk; the reply's `upload_id` in the next ones |
| `sample_rate` | The filter's rate, the same in every chunk |
| `total_taps` | The filter's length, the same in every chunk |
| `offset` | Where this chunk starts: 0, then the reply's `received_taps` |
| `taps` | Base64 (standard alphabet, padded) of the taps as little-endian 32-bit floats, at most 8192 taps (`MAX_UPLOAD_CHUNK_TAPS`) |

The default 8192-tap filter travels in one request. The reply is
`filter_upload: {upload: {upload_id, sample_rate, total_taps, received_taps,
hash}}`; `hash` appears once every tap arrived: the 64-bit FNV-1a hash of the
taps' little-endian bytes, as 16 lowercase hex digits (`taps_hash_hex`). A
chunk out of order, past the end, or with another rate or length is refused
(`invalid_filter`) and the upload stays as it was, to be sent again. A filter
found silent once complete is refused and forgotten. The renderer keeps at
most 64 uploads (64 MiB of taps) that no `set_output_filters` has used yet
(`upload_limit` past that), and forgets one no request touched for two
minutes (`unknown_upload` afterwards). Uploading is accepted in and out of a
session: it only stores the taps. The contract crate provides `encode_taps`,
`decode_taps` and `taps_hash_hex`.

### Setting filters

`set_output_filters: {expected_revision, outputs: [{output_id, filter}]}`
replaces the filters of the outputs it names, as `set_outputs` does their
gains and delays: the revision is checked under the live-state writer lock,
an unknown output, a duplicate, or an unknown or incomplete upload refuses
the whole command, and the outputs it names change together in one
publication. `filter` is an upload's `upload_id`, or `null` for no filter at
all (a file the layout names included). It is refused during any session,
the caller's own included (`busy`): the session's snapshot would no longer
describe what plays. An applied upload is consumed.

The filters are live state, like `set_outputs`: they light the Studio's Save
button and reach the saved configuration only through it. The Save writes
each one as a mono 32-bit float WAV file under `room-correction/<profile>/`
next to `config.yaml` and names it in the speaker's `correction_filter`; the
speaker then plays that file, the same taps (same hash). A Reload or a
profile switch drops filters that were not saved, as it drops every unsaved
edit, and so does an engine handoff (a restart, mpv taking over). The
Studio shows a live filter as "Measured filter, not saved". A filter follows
its output's name: layout edits that move, add, remove or replace speakers
keep it on the speaker of that name, and choosing a file for that speaker
in the Studio replaces it.

### Filters in the snapshot

From version 8 each output carries `filter` when it has one:

| Field | Meaning |
| --- | --- |
| `origin` | `file` (a file the layout names) or `live` (an upload not saved yet) |
| `file` | The file, for `origin: file` |
| `state` | `pending` (being read and prepared), `loaded`, or `error` (the output plays unfiltered) |
| `sample_rate`, `taps`, `hash` | The filter at its own rate, as uploaded or read; a file's once loaded |
| `error` | Why it could not be used |

A client checks that an output plays what it uploaded by comparing `hash`.
The snapshot revision follows the filters themselves, not their loading: a
filter that finishes loading does not end a session.

### Measuring without the filters

`begin` takes `bypass_output_filters: true` (omitted when false): while the
session holds, every output plays unfiltered, through the correction stage's
identity bank, as with the `output_correction` option off: the outputs keep
their latency, so a measurement without the filters lines up with one
through them. The filters come back when the session ends, however it ends:
`end`, the connection closing, a configuration change, or the lease running
out, which the audio thread checks on its own. The session's snapshot reports
`output_filters_bypassed: true` (omitted when false), and the Studio shows
that a measurement session holds the filters off.

A change of filters reaches the longest part of a filter (its 8192-sample
partitions) only after up to two of their periods, about 340 ms at 48 kHz:
longer than the 250 ms of silence before a sweep. So in a version-8 session
a `start` is refused with `filters_settling`, before anything plays, until
every output plays through the filters the session asked for alone (the
upload loaded, the bypass in place); start again shortly with the same
`signal_id`. A version-7 session is never refused this way.

### Bass-managed sweeps

`start` takes `bass_managed: true` (omitted when false; echoed in the
progress): each target's sweep then plays as a source at that output's
layout position, split by the crossover and panned with each band's gains
like programme audio, so bass management gives a subwoofer the low band. The
reference sweeps stay direct. The source sits on its speaker, so what is
meant for objects moving through the room is left out: the distance
diffusion is off, and each band's gains are scaled to unit energy, which
takes out the distance model's attenuation; the speaker's band then plays at
the sweep's peak, as a direct sweep would. A target's sweep may start below
its own `freq_low_hz` (down to 20 Hz), since the crossover gives that part to
the subwoofers, and the -6 dBFS ceiling is checked against the loudest
unmuted output's gain, which the sweep may reach through the panning. The
sweep still goes through every output's gain, delay and mute, and through
the filters unless the session bypasses them. The segment timing is the
same as a direct sweep's, but the crossover's response is in the target
segments and not in the direct reference sweeps around them: the LR4
crossover's phase, and the FIR crossover's whole latency (the crossover part
of the renderer's reported output latency) as a delay of the target.


