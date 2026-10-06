# RFC: a reliable local transport for Studio ↔ engine control

Status: **proposal** for the long-term part of #680. Nothing here is built.
The interim part (control errors, state generation, contract revision,
payload shapes) shipped in #709 and stays.

## Problem

Studio drives the engine over OSC/UDP, and the control plane has grown a
reliability layer of its own on top of datagrams, one feature at a time
(paths are under `omniphony-renderer/` unless stated):

| Mechanism | Engine | Studio | About |
|---|---|---|---|
| Registration, heartbeat, epoch, client timeout | `orender_engine/src/osc.rs`, `osc/client_registry.rs` | `core/src/osc/mod.rs` | 350 lines |
| State generation and `/control/state/refresh` | `osc/client_registry.rs`, `osc/transport.rs` | `core/src/osc/state_sync.rs` | 260 lines |
| Snapshot split into datagram-sized parts | `osc/export.rs` (`MAX_STATE_DATAGRAM`) | `snapshot_complete` | 150 lines |
| Gain table in 1 KB chunks, NACK and resend | `runtime_control/src/osc.rs`, `osc/dispatch.rs` | `core/src/osc/apply.rs` | 320 lines |
| `SO_SNDBUF` raised for large datagrams | `osc/transport.rs` | `core/src/osc/mod.rs` | 60 lines |
| Nesting bound before decode | `osc/decode.rs` | `core/src/osc/mod.rs` | `osc-contract/src/nesting.rs` |

Some traffic still has no recovery at all. An HRTF upload is sent as 32 KB
chunks paced by a sleep, with no resend (`core/src/host/commands/sofa.rs`),
and the engine aborts on the first sequence gap.

A control is answered only by `/state/control_error` when it fails. A control
that succeeded and one that was lost look the same until the state changes.
Replies carry no request id: only the backend-file requests have one.

Two more things surfaced while taking this inventory:
- The engine drops a client after 15 s (`CLIENT_TIMEOUT`). The contract
  document says 10 s, which is Studio's own staleness timeout.
- The OSC socket binds `0.0.0.0` and accepts `quit` and `yield_port` from
  anyone who can reach the port.

## Proposal

**OSC 1.0 stream framing over loopback TCP, on the same port number as the
OSC/UDP socket.** Studio moves to it, telemetry included. Public automation
and remote clients stay on UDP.

### Why this transport

- **Same messages.** OSC 1.0 defines a stream transport: each packet is
  preceded by its size as a big-endian int32. Every address, argument shape,
  JSON payload, the dispatcher (`osc/dispatch.rs`), the command table and
  Studio's parser and `OscEvent` stay as they are. A TCP packet is decoded
  with the same `rosc` call and the same nesting bound, and dispatched
  exactly like a datagram. The contract tests keep applying.
- **Same identity.** The instance is identified by its port today: yield,
  resume, the standby handoff, `OMNIPHONY_OSC_PORT`, Studio's target
  `host:port`. TCP and UDP port spaces are separate, so the engine can
  listen on TCP *n* next to UDP *n*, and nothing has to agree on a socket
  path or a pipe name.
- **No new platform code.** `std::net::TcpListener` is the same on Linux,
  macOS and Windows. A Unix socket has no std support on Windows, and a
  named pipe needs overlapped I/O there. Both would also need a rendezvous
  name derived from the port anyway.
- **Local by default.** The listener binds `127.0.0.1`. Only processes on
  the machine can send `quit` or `yield_port` through it. A remote Studio
  keeps using UDP, exactly as today.

The alternative the issue mentions, JSON request/response, would replace the
165 control addresses and the 77 state addresses with a second vocabulary to
keep in step. Framing the existing messages gets the reliability without
that cost.

### What TCP gives, and what goes away for a TCP client

| Today, on UDP | On TCP |
|---|---|
| Register, heartbeat every 5 s, dropped after 15 s | Connect. The connection is the session; closing it unregisters. A heartbeat is kept only to measure round trip |
| Snapshot split into parts, `snapshot_complete` | One packet: no size limit beyond a sanity bound |
| Generation + `/control/state/refresh` to catch lost state | State arrives in order and is never lost. The generation stays as a consistency marker; refresh becomes a no-op |
| Gain table: 1 KB chunks, NACK, resend, 5 s re-subscribe repair | One packet per table version |
| HRTF upload paced by sleeps, aborted on a gap | Chunks back to back; order and delivery are TCP's |
| `SO_SNDBUF` workaround | Not needed |

**Request/response.** A new session message, `/omniphony/sync [token]`,
which the engine answers with `/omniphony/sync/ack [token]` once every
earlier packet on the connection has been dispatched. One address pair
gives the barrier a control API needs, and no per-message id has to be
threaded through 164 handlers.

What the ack guarantees is **dispatch**, not completion. When it arrives,
every earlier control has gone through its handler and has one of three
outcomes:
- it was applied, and the state it changed has been published, so that
  state reached this client before the ack;
- it was refused, and its `control_error` arrived before the ack;
- it started asynchronous work, and that work has not finished.

The third case is real: a layout or speaker change starts a recompute on
the `render-backend-recompute` worker and returns at once. That build can
still fail after the ack. Its completion is reported the way it is today:
`/state/speakers/recomputing` goes to 1 when the build starts and back to 0
when it ends, followed by `/state/speakers/recompute_error` (empty on
success). A client that needs completion waits for `recomputing` to return
to 0 after the ack. It does not take the ack as success. Making the barrier
wait for the worker would block every later packet on the connection for
the length of a table build, telemetry subscriptions included.

UDP clients (automation, a remote Studio, mpv's overlay script) keep the
whole current mechanism. Nothing is removed from the UDP path.

### Engine design

- **Acceptor thread** `osc-control-tcp`: accepts on `127.0.0.1:<rx port>`.
  For each connection it spawns a reader thread and a writer thread. Clients
  are few (a Studio, a test), so one thread each is cheaper to reason about
  than an event loop.
- **Reader:** reads size-prefixed packets with a size bound (1 MiB, which a
  whole snapshot fits in), and hands them to the listener thread through the
  channel the datagrams now go through too. The listener dispatches both one
  at a time, so its state stays single-threaded and a connection's packets
  are handled in order. The decode, the nesting bound and
  `handle_control_message` are the datagrams'. The dispatcher's `src:
  SocketAddr` becomes a `Peer`: `Udp(SocketAddr)` or `Tcp(connection)`, and
  every reply goes through `Peer::send`.
- **Writer:** each TCP client owns a bounded queue (8 MiB of encoded
  packets) and a writer thread. Fan-out (`send_filtered`) pushes and never
  writes to a socket. When the queue is full, telemetry is dropped, as a
  datagram would be. State is never dropped: the client is too slow, so it is
  disconnected, and on reconnecting gets a fresh snapshot. Either way the
  publisher never waits.
  - Today a publisher holds the clients mutex and the publication lock while
    it calls `send_to`, a non-blocking datagram send. A blocking stream write
    there would stall the telemetry thread. The queue keeps every lock hold
    as short as it is now.
  - The render thread is unaffected: it never touches the registry (#670).
- **Registry:** `HashMap<Peer, OscClientState>`. Interest flags (metering,
  diag, gain-table targets) stay per client. A TCP client is not timed out;
  it goes when its connection closes. It receives everything a datagram
  client does, telemetry included, on the one connection: one socket, and
  no reply port to agree on. Telemetry is marked droppable, so it keeps the
  loss semantics it has on UDP.
- **Yield/resume:** yielding the port closes the TCP listener with the UDP
  socket, and a resume reopens both. Clients reconnect, as they re-register
  today.
- **Embedded (liborender in mpv):** the same listener starts wherever OSC
  is enabled. The embedded engine is never yieldable, which is unchanged.

### Studio design

- `core/src/osc/` gains a TCP link next to the UDP socket. When the target
  host is loopback, Studio connects over TCP. It falls back to UDP when the
  connection is refused (an engine from before this change) or when the host
  is remote.
- The link is chosen once per connection. `ControlTx` and `OscEvent` do not
  change, so neither do the panels nor the architecture ratchet.
- On TCP, Studio skips the state-sync retry loop, the gain-table NACK timer
  and the HRTF pacing. They stay in place for the UDP fallback.
- The Tauri Studio is not ported: it is deprecated (#677).

### Compatibility

- **Contract revision.** `CONTRACT_REVISION` 1 → 2 for the new session
  addresses (`/omniphony/sync` and its ack) and the stream transport. The
  address fingerprint test moves with it. No separate capability flag: a
  revision-2 engine whose TCP bind failed refuses the connection, which a
  client handles the same way as an older engine.
- **Old Studio, new engine:** UDP, as today.
- **New Studio, old engine:** the TCP connect is refused, so Studio stays on
  UDP.

### Tests

- The engine's UDP dispatch tests (`osc/dispatch.rs`, `listening_sender`)
  are parameterized over the transport, so every contract test runs on both.
- A slow-reader test: a TCP client that never reads is disconnected, and
  telemetry publication timing does not move.
- A sync test: controls followed by a sync arrive applied or refused before
  the ack. A layout change followed by a sync gets its ack with
  `recomputing = 1` already sent, and the build's outcome
  (`recomputing = 0`, `recompute_error`) after it.
- Studio: the conformance test against `shapes::STATE` runs on a TCP-fed
  parser too.
- The persistence-policy tripwire (`runtime_control/tests/persistence_policy.rs`)
  is unaffected: writes stay behind the same functions.

## Plan

Each step can be merged on its own, and nothing changes for a user until
step 2.

1. **Engine:** the listener, framing, per-client writer queues, the
   `ClientId` refactor and the sync pair; contract revision 2; tests on both
   transports. Studio still uses UDP.
2. **Studio:** the TCP link with the UDP fallback; skip the UDP-only repair
   loops on TCP.
3. **Large payloads over TCP:** gain tables in one packet, and the HRTF
   upload without pacing.
4. **Housekeeping:** fix the 10/15 s timeout drift in the contract doc, and
   decide whether the UDP control socket should bind loopback by default
   (a remote Studio would then need an opt-in).

## Open questions

- Does mpv's overlay or any user's automation rely on controls only Studio
  sends today? If so, those stay reachable over UDP; nothing above removes
  them.
- Telemetry for a TCP client: settled in step 1 (#763). It goes over the
  stream, droppable when the client falls behind, rather than over UDP to a
  reply port.
