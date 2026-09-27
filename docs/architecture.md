# Architecture

## Roles

Control direction is independent of which side opens the network connection:

- **Driving Mac**: owns the keyboard and trackpad. Its setup records which screen edge leads to the other Mac.
- **Following Mac**: replays the input sent by the driving Mac.
- **Waiting Mac**: listens for a network connection.
- **Connecting Mac**: opens the connection to the waiting Mac.

Either Mac can wait or connect. Exactly one Mac must drive each session.

## Layers

| Layer | Module | Responsibility |
|---|---|---|
| Process entry | `main.rs` | Opens the menu-bar app by default and retains optional CLI commands |
| Native interface | `app` | AppKit menu, setup window, permission prompts, pairing prompts and launch-at-login control on the main thread |
| Controller | `controller` | Owns the background Tokio runtime, saved setup and typed command/event channels |
| Connection service | `service` | Shared listen/connect, pairing, trust and input-sharing orchestration used by both UI and CLI |
| Launcher | `launcher` | Relaunches a bundled CLI invocation as `Daisy.app` so macOS applies the app's permissions; stops it when its terminal goes away |
| Identity | `identity`, `peers`, `trust` | This Mac's long-term key, paired Macs and how long each stays trusted |
| Session | `session` | Noise handshake, encrypted framing and split send/receive halves |
| Trust | `pairing` | Pinned keys and pairing through a one-time code |
| Wire | `protocol` | Messages; see [protocol.md](protocol.md) |
| Sharing | `share` | Driving and following loops, bounded queues, heartbeat and cleanup |
| Clipboard | `clipboard` | What to send when control crosses, echo prevention, chunking and reassembly; `macos::pasteboard` reads and writes the pasteboard |
| Decisions | `input`, `swipe`, `shake`, `trust` | Routing, edge crossing, held-input release, swipe pacing, pointer-shake recognition and trust duration |
| macOS | `macos::*` | Event tap, event posting, pointer pinning, swipe synthesis, Mission Control shortcuts and permissions |

Pure decision modules contain no macOS calls and are unit tested directly. The macOS modules carry those decisions out; they do not decide them. See [macos.md](macos.md).

## Threads and tasks

- **AppKit main thread**: owns every native menu, window and control. It never waits on the network or reads trust files from an event callback.
- **Controller thread**: owns a multithreaded Tokio runtime. Typed channels carry `Command` values from AppKit and `Event` values back to it.
- **Network tasks**: run Noise, pairing, trust and sharing. The menu-bar app and CLI call the same `service` functions, so neither duplicates protocol or authorization decisions.
- **Event-tap thread** on a driving Mac: owns the CoreFoundation run loop. Its callback makes decisions under nonblocking locks and hands messages to the runtime through a bounded channel. It never waits, performs I/O, starts a process or lets a panic escape into CoreGraphics.
- **Swipe-poster thread** on a macOS 27 following Mac: replays swipe steps at least 16 ms apart and coalesces progress that arrives faster, preserving order without stalling the session.

The network queues and event-tap queue are bounded. If callback state is contended or the local input queue fills, the callback passes input through locally, reclaims local routing when possible and raises a lock-free overflow signal. The runtime then ends the session. Local control and explicit disconnect win over lagging input.

## Session flow

1. The controller starts either a listener or one outgoing connection from saved `SessionSettings`.
2. Noise `XX` establishes an encrypted channel and exposes the remote static key.
3. The trust layer accepts an already-pinned key or, only while both Macs explicitly allow pairing, runs SPAKE2 using the six-digit code as input.
4. Both sides declare whether they drive. Exactly one driving side is required.
5. The driving Mac's event tap observes local input. While control is local, events pass through untouched.
6. Pushing the pointer through the shared edge hides and pins the driving pointer, then sends `Enter` with the crossing position. Control never crosses while a mouse button is held.
7. Input is swallowed locally and forwarded. A key pressed before crossing keeps its release on the driving Mac, so it cannot become stuck remotely.
8. The following Mac moves its pointer to the corresponding edge position. Pushing it out through that edge releases held input and sends `Leave`; the driving pointer reappears at the same proportional position.
9. Whenever control crosses, the Mac giving it up reads its clipboard on a blocking thread and sends it behind input and heartbeats. Chunks are acknowledged and at most four are in flight, so a large image never holds up input. The receiver accepts one snapshot per crossing and writes it once the whole snapshot has arrived.
10. Control-Option-Command-Escape, explicit stop, trust revocation, a dropped connection, three seconds of silence or queue overload takes control back immediately.

Whatever ends following replay releases every key and button that Mac still considers held.

## Reconnecting

The Mac set to connect runs `service::connect` as a loop. Once a session has run, a drop is followed by a wait from `reconnect::waits()` and a fresh attempt: TCP connect, Noise handshake, a check that the key matches the Mac of the first session, the trust check, then a new session. `reconnect::retryable` retries only failures of the connection itself (unreachable, reset, silent, timed out); trust, key, protocol, setup and local file errors stop the loop. The first attempt is never retried, and reconnects never pair. The listening Mac needs nothing extra: it serves one connection at a time and accepts the next one when a session ends. A peer that closes after more than the silence limit counts as a lost connection, not a deliberate stop, so single-session trust keeps its reconnect grace.

## Saved state

`~/Library/Application Support/daisy/` holds:

- `identity`: this Mac's long-term private key pair.
- `peers.toml`: one entry per paired Mac, including key, name, trust policy, pairing time and last-seen time.
- `settings.toml`: the menu-bar app's last connection, control and trust choices, written atomically with mode `0600`.

`peers.toml` is the trust authority. Every change locks it, rereads it and writes it atomically. An active session watches the paired key, so a change from the menu-bar app or another process revokes control promptly. The UI aborts its current task immediately when its **Forget** action is used; the file watcher remains the external-edit backstop.

`--home` or `DAISY_HOME` points all three files somewhere else and can be used to run two identities on one Mac for testing.
