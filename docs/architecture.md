# Architecture

## Control and layout

Both peers capture physical input and can replay remote input. The system being used supplies input; touching the peer takes control there immediately. Connection direction is independent of control.

Both listen and advertise with Bonjour. Paired public keys elect one connection opener; anonymous pairing advertisements use their random nonces. A direct address remains available.

Each system has a screen-edge choice for where its peer sits, saved with the paired key along with when it was chosen. After trust negotiation both send `Layout`; the choice made most recently wins, with the initiator winning a tie, and the other system uses its inverse. Both save the agreed relation, so changing control or reconnecting preserves the arrangement.

## Layers

| Layer | Module | Responsibility |
|---|---|---|
| Process entry | `main.rs` | Opens the menu-bar app by default and retains optional CLI commands |
| Native interface | `app` | AppKit menu, setup window, permission prompts, pairing prompts and launch-at-login control on the main thread |
| Controller | `controller` | Owns the background Tokio runtime, saved setup and typed command/event channels |
| Connection service | `service` | Shared listen/connect, pairing, trust and input-sharing orchestration used by both UI and CLI |
| Launcher | `launcher` | Relaunches a bundled CLI invocation as `Daisy.app` so macOS applies the app's permissions; stops it when its terminal goes away |
| Identity | `identity`, `peers`, `trust` | This system's long-term key, paired peers and how long each stays trusted |
| Session | `session` | Noise handshake, encrypted framing and split send/receive halves |
| Trust | `pairing` | Pinned keys and pairing through a one-time code |
| Wire | `protocol` | Messages; see [protocol.md](protocol.md) |
| Control | `control` | Ownership, simultaneous-use settling and shared-layout decisions |
| Sharing | `share` | Bidirectional sharing, generation-stamped control claims, bounded queues, heartbeat and cleanup |
| Latency | `latency` | Round-trip average from heartbeat pings and pongs, reported with who has control |
| Clipboard | `clipboard` | What to send when control crosses, echo prevention, chunking and reassembly; `macos::pasteboard` reads and writes the pasteboard |
| Decisions | `input`, `swipe`, `shake`, `trust` | Routing, edge crossing, held-input release, swipe pacing, pointer-shake recognition and trust duration |
| macOS | `macos::*` | Event tap, event posting, pointer pinning, swipe synthesis, Mission Control shortcuts and permissions |

Pure decision modules contain no macOS calls and are unit tested directly. The macOS modules carry those decisions out; they do not decide them. See [macos.md](macos.md).

## Threads and tasks

- **AppKit main thread**: owns every native menu, window and control. It never waits on the network or reads trust files from an event callback.
- **Controller thread**: owns a multithreaded Tokio runtime. Typed channels carry `Command` values from AppKit and `Event` values back to it.
- **Network tasks**: run Noise, pairing, trust and sharing. The menu-bar app and CLI call the same `service` functions, so neither duplicates protocol or authorization decisions.
- **Event-tap thread**: owns the CoreFoundation run loop. Its callback makes decisions under nonblocking locks and hands messages to the runtime through a bounded channel. It never waits, performs I/O, starts a process or lets a panic escape into CoreGraphics.
- **Swipe-poster thread** on macOS 27: replays swipe steps at least 16 ms apart and coalesces progress that arrives faster, preserving order without stalling the session.

The network queues and event-tap queue are bounded. If callback state is contended or the local input queue fills, the callback passes input through locally, reclaims local routing when possible and raises a lock-free overflow signal. The runtime then ends the session. Local control and explicit disconnect win over lagging input.

## Session flow

1. The controller starts automatic listening and browsing, or an optional direct connection from saved `SessionSettings`.
2. Noise `XX` establishes an encrypted channel and exposes the remote static key.
3. The trust layer accepts an already-pinned key or, only while both peers explicitly allow pairing, runs SPAKE2 using the six-digit code as input.
4. Both sides exchange `Layout`; the most recent choice, or the initiator's on a tie, and its inverse define both crossing directions.
5. The event tap on the system in use observes local input. While control is local, events pass through untouched.
6. Pushing the pointer through the shared edge hides and pins the local pointer, then sends `SharedEnter` with the crossing position. Control never crosses while a mouse button is held.
7. Input is swallowed locally and forwarded. A key pressed before crossing keeps its release on the system it was pressed on, so it cannot become stuck remotely.
8. The peer moves its pointer to the corresponding edge position. Pushing it out through that edge releases held input and sends `SharedLeave`; the local pointer reappears at the same proportional position.
9. Whenever control moves, whether through the edge or because someone started using the other system, the peer giving it up reads its clipboard on a blocking thread and sends it behind input and heartbeats. Chunks are acknowledged and at most four are in flight, so a large image never holds up input. The receiver accepts one snapshot per crossing and writes it once the whole snapshot has arrived.
10. Control-Option-Command-Escape returns control to the system it was pressed on. Explicit stop, trust revocation, a dropped connection, three seconds of silence or queue overload takes control back immediately.

Whatever ends replay of remote input releases every key and button that system still considers held.

## Reconnecting

Nearby paired peers reconnect automatically: after a session ends, both return to listening and advertising with Bonjour, and the elected opener connects again as soon as it sees the peer, running the full handshake and trust check. A direct address instead runs `service::connect` as a loop. Once a session has run, a drop is followed by a wait from `reconnect::waits()` and a fresh attempt: TCP connect, Noise handshake, a check that the key matches the peer of the first session, the trust check, then a new session. `reconnect::retryable` retries only failures of the connection itself (unreachable, reset, silent, timed out); trust, key, protocol, setup and local file errors stop the loop. The first attempt is never retried, and reconnects never pair. The listening peer needs nothing extra: it serves one connection at a time and accepts the next one when a session ends. A peer that closes after more than the silence limit counts as a lost connection, not a deliberate stop, so single-session trust keeps its reconnect grace.

## Saved state

`~/Library/Application Support/daisy/` holds:

- `identity`: this system's long-term private key pair.
- `peers.toml`: one entry per paired peer, including key, name, trust policy, pairing time and last-seen time.
- `settings.toml`: the menu-bar app's last connection, control and trust choices, written atomically with mode `0600`.

`peers.toml` is the trust authority. Every change locks it, rereads it and writes it atomically. An active session watches the paired key, so a change from the menu-bar app or another process revokes control promptly. The UI aborts its current task immediately when its **Forget** action is used; the file watcher remains the external-edit backstop.

`--home` or `DAISY_HOME` points all three files somewhere else and can be used to run two identities on one system for testing.

## Ownership handoff

Physical events on a receiving system pass locally on their first callback and claim control; momentum scrolling after a trackpad flick never claims. A monotonically increasing generation identifies the owner; equal generations favor the Noise initiator. Repeated local claims are limited to once per 150 ms during simultaneous use, while local activity excludes remote injection immediately. Claims release held keys, buttons, modifiers and swipes. Every shared input and crossing carries its generation, so queued input from the previous owner is discarded.

Posted input and swipe shortcut events carry a Daisy marker that the event tap excludes from ownership and forwarding. The callback uses only cached state, nonblocking locks and bounded queue operations. Both sides exchange heartbeat messages and unwind capture and held-input state on cancellation or connection loss.
