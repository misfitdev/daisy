# Architecture

How Daisy routes input, coordinates a group and saves state. For setup and daily tasks, use the [User guide](usage.md).

## Control and layout

A group is up to eight systems, each holding one encrypted session with every other. Every member captures physical input and can replay remote input. The system being used supplies input; touching another member takes control there immediately. Connection direction is independent of control.

Every member listens and advertises with Bonjour, and connects to each trusted member it finds; for each pair, the lower public key opens the connection. For pairing, a group member opens the connection to a system with no peers; between two systems with no peers, the one open to pairing longer opens, and random nonces break a tie. A direct address remains available.

Each member shares its displays and one agreed arrangement: an offset per member that places its displays in a shared space, never overlapping another member's. The greatest `(version, author)` wins everywhere, so every member converges on the same arrangement whoever changes it. A member that joins without a position is placed on the side its pairing chose, clear of the rest. The arrangement is saved, so a member returns where it was placed.

## Layers

| Layer | Module | Responsibility |
|---|---|---|
| Process entry | `main.rs` | Opens the menu-bar app by default and retains optional CLI commands |
| Native interface | `app` | AppKit menu, setup window, permission prompts, pairing prompts and launch-at-login control on the main thread |
| Controller | `controller` | Owns the background Tokio runtime, saved setup and typed command/event channels |
| Connection service | `service` | Shared listen/connect, pairing, trust and input-sharing orchestration used by both UI and CLI |
| Launcher | `launcher` | Relaunches a bundled CLI invocation as `Daisy.app` so macOS applies the app's permissions; stops it when its terminal goes away |
| Identity | `identity`, `device`, `peers`, `trust`, `introduce` | This system's long-term keys, trusted peers, how long each stays trusted, and signed introductions and revocations |
| Session | `session` | Noise handshake, encrypted framing and split send/receive halves |
| Trust | `pairing` | Pinned keys and pairing through a one-time code |
| Wire | `protocol` | Messages; see [protocol.md](protocol.md) |
| Control | `control` | Group-wide ownership and simultaneous-use settling |
| Layout | `layout` | Display geometry, the agreed arrangement, snapping, and where the pointer goes next |
| Sharing | `share` | One input core for the group, with a link per member: claims, routing, arrangement, bounded queues, heartbeat and cleanup |
| Developer diagnostics | `diagnostics`, `macos::diagnostics` | Dynamic trace requests, bounded event capture and encrypted streaming, local collector socket, foreground observations |
| Latency | `latency` | Round trips per link from heartbeat pings and pongs: a recent average and percentiles |
| Clipboard | `clipboard` | Copy IDs, offers and requests, what to send when control crosses to an older peer, echo prevention, chunking and reassembly; `macos::pasteboard` reads and writes the pasteboard |
| Copied files | `files`, `file_transfer`, `file_cache`, `macos::file_pasteboard` | Offer leases, separately authenticated bulk connections, atomic staging, bounded clipboard cache and native progress |
| Decisions | `input`, `swipe`, `shake`, `control`, `trust`, `install`, `setup` | Routing, edge crossing, held-input release, swipe pacing, pointer-shake recognition, trust duration, moving to Applications and the permission walkthrough |
| macOS | `macos::*` | Event tap, event posting, pointer pinning, swipe synthesis, Mission Control shortcuts, permissions and moving the app |

Pure decision modules contain no macOS calls and are unit tested directly. The macOS modules carry those decisions out; they do not decide them. See [macos.md](macos.md).

## Staged updates

The staged update transaction lives in `install::update`. It copies and
verifies complete bundles before stopping sharing, then delegates process
exit, port availability, atomic bundle exchange and reopening to
`macos::update`. A retained old executable runs the helper. Startup is committed
only after the replacement's UI and controller are ready; when sharing was
enabled, the listener must have started. Failure stops the replacement before
restoring and reopening the previous bundle. Interrupted cutovers retain a
complete installed path and recover through the old helper. Device keys,
peer trust, settings and arrangement remain in the existing data folder.

## Threads and tasks

- **AppKit main thread**: owns every native menu, window and control. It never waits on the network or reads trust files from an event callback.
- **Controller thread**: owns a multithreaded Tokio runtime. Typed channels carry `Command` values from AppKit and `Event` values back to it.
- **Network tasks**: run Noise, pairing, trust and sharing. The menu-bar app and CLI call the same `service` functions, so neither duplicates protocol or authorization decisions.
- **Event-tap thread**: owns the CoreFoundation run loop. Its callback makes decisions under nonblocking locks and hands messages to the runtime through a bounded channel. It never waits, performs I/O, starts a process or lets a panic escape into CoreGraphics.
- **Swipe-poster thread** on macOS 27: replays swipe steps at least 16 ms apart and coalesces progress that arrives faster, preserving order without stalling the session.

The network queues and event-tap queue are bounded. If callback state is contended or the local input queue fills, the callback passes input through locally, reclaims local routing when possible and raises a lock-free overflow signal. The runtime then ends the session. Local control and explicit disconnect win over lagging input.

The connection session owns the lifetime of every background input core. Core
tasks may hold group handles, but those handles cannot keep the session alive.
Stopping sharing cancels the cores, drops their transports and releases held
input through the existing cancellation cleanup.

Keyboard modifier transitions are tracked on both sides. A forwarded key keeps
held modifier flags even if its own event omits them. Replay distinguishes
modifier presses from releases, including releasing one side while the other
side remains held.

## Session flow

1. The controller starts automatic listening and browsing, or an optional direct connection from saved `SessionSettings`.
2. Noise `XX` establishes an encrypted channel and exposes the remote static key. Both sides exchange and verify `DeviceProof`, signed over its handshake hash and sender role with their Secure Enclave device keys, before settling trust.
3. The trust layer accepts an already-pinned key or, only while both peers explicitly allow pairing, runs SPAKE2 using the six-digit code as input.
4. Both sides exchange `Layout`, then catch each other up on introductions and revocations, and the link joins the input core. The first link starts the core: one event tap, one injector and one owner of control for every link; the last link to end stops it.
5. The event tap on the system in use observes local input. While control is local, events pass through untouched.
6. Pushing the pointer off one of this system's displays toward another member's display, within 40 points, hides and pins the local pointer, then sends `Enter` to that member with the exact entry point. Control never crosses while a mouse button is held, and never onto a locked member.
7. Input is swallowed locally and forwarded. A key pressed before crossing keeps its release on the system it was pressed on, so it cannot become stuck remotely.
8. The member being driven moves its pointer within its displays. Pushing it off them toward another display releases held input and sends `Leave` naming the next system and point; the system in control sends `Enter` to that system, or puts its own pointer back at that point.
9. Whenever control moves, whether across displays or because someone started using another member, the system giving it up reads its clipboard on a blocking thread and sends it behind input and heartbeats. Chunks are acknowledged and at most four are in flight, so a large image never holds up input. The receiver writes a snapshot once the whole of it has arrived. Between systems that both support clipboard IDs, the system giving up control first offers the copy's ID, and the snapshot follows only if the receiver lacks that copy; the ID travels with the copy, so no system is sent a copy it already has. A received copy is never offered back to its supplying peer while it remains on the clipboard, and this system rejects offers of its own copies so a delayed handoff cannot replace a newer local copy. When the pointer moves from one driven member on to another, the driver accepts the copy the first member offers and passes it on. With an older member, the snapshot goes on each crossing and the receiver accepts one per crossing.
10. Control-Option-Command-Escape returns control to the system it was pressed on. Explicit stop, trust revocation, a dropped link with the member in control, three seconds of silence or queue overload takes control back immediately.

Whatever ends replay of remote input releases every key and button that system still considers held. While any link runs, the system holds off idle sleep; control arriving wakes its display. The event tap records physical input in an atomic timestamp. Once per second, the system in control broadcasts `Activity` only if that timestamp changed. Each unlocked member accepts it only from the current owner at the current generation, refreshes a timed macOS user-activity assertion, and posts a do-nothing modifier event so its screen saver idle time also restarts; see [macOS internals](macos.md). Heartbeats do not refresh display activity; when input stops, normal display sleep, screen saver and lock timers apply.

## Reconnecting

While sharing remains enabled, nearby paired peers reconnect after network drops when trust remains valid and Bonjour can discover an advertising peer. The elected opener repeats the full handshake, device proof and trust checks. **Stop Sharing** ends connections without triggering reconnect. The native app adds explicit address attempts to the same listening and discovering group, sharing its input core and preserving existing links.

The command-line `connect` command runs `service::connect` as a loop. Once a session has run, a drop is followed by a wait from `reconnect::waits()` and a fresh attempt: TCP connect, Noise handshake, a check that the key matches the peer of the first session, the trust check, then a new session. `reconnect::retryable` retries only failures of the connection itself (unreachable, reset, silent, timed out); trust, key, protocol, setup and local file errors stop the loop. The first attempt is never retried, and reconnects never pair. The listening peer needs nothing extra: it accepts every connection and runs each as its own link. A peer that closes after more than the silence limit counts as a lost connection, not a deliberate stop, so single-session trust keeps its reconnect grace.

## Saved state

`~/Library/Application Support/daisy/` holds:

- `identity`: this system's long-term private key pair.
- `device-identity`: a reference to the non-exportable Secure Enclave P-256 key in the Data Protection Keychain.
- `trust-v5/peers.toml`: one entry per trusted peer, including key, name, trust policy, pairing time, last-seen time, its signing key and who introduced it.
- `trust-v5/revocations.toml`: signed revocations this system knows, to pass on.
- `trust-v5/arrangement.toml`: the newest group arrangement it saw.
- `trust-v5/arrangement-confirmation.toml`: checked member display sets and
  their arrangement version, retained across reconnects and restarts.
- `stats.toml`: each running link's round trips, for `daisy stats`.
- `settings.toml`: the menu-bar app's last connection, control and trust choices, written atomically with mode `0600`.
- `last-update-check.toml`: when the last successful update check ran and the newer release it found, if any, written atomically with mode `0600`.

`trust-v5/peers.toml` is the trust authority. Every change locks it, rereads it and writes it atomically. An active session watches the paired key, so a change from the menu-bar app or another process revokes control promptly. The UI aborts its current task immediately when its **Forget** action is used; the file watcher remains the external-edit backstop.

`--home` or `DAISY_HOME` points all of these somewhere else and can be used to run two identities on one system for testing.

## Ownership handoff

Physical events on a receiving system pass locally on their first callback and claim control; momentum scrolling after a trackpad flick never claims, and stays on the system the flick began on. A touch on a trackpad that is not a swipe, such as a resting palm, sends gesture events that never claim control. A claim names the claimant and a generation, and goes to every member; every member keeps the greatest `(generation, key)`, so all agree on one owner. Repeated local claims are limited to once per 150 ms during simultaneous use, while local activity excludes remote injection immediately. Claims release held keys, buttons, modifiers, swipes and scrolls. Every shared input and crossing carries its generation, so queued input from the previous owner is discarded.

Posted input and swipe shortcut events carry a Daisy marker that the event tap excludes from ownership and forwarding. The callback uses only cached state, nonblocking locks and bounded queue operations. Both sides exchange heartbeat messages and unwind capture and held-input state on cancellation or connection loss.

## Verified release installation

`install::release` selects a newer stable release and verifies both its
compatibility manifest and architecture-specific ZIP. Anonymous HTTPS reads
have request and size limits. Sigstore verification checks certificate chains,
transparency evidence and the exact release workflow/tag identity; the signed
SLSA statement must also name the selected asset and SHA-256 digest. Missing or
invalid metadata prevents staging.

Archives are extracted into a private temporary directory after checking every
path, entry type and expanded size. Links, special files, duplicate paths and
paths outside the app are rejected. The downloaded app's signed build metadata
must match the verified manifest, and its Developer ID publisher must match
the installed copy. The existing staging helper then owns restart, readiness
confirmation and rollback. A failed download or verification leaves the
installed app running. `macos::update::request_release` starts a separate
installer worker for policy callers; it does not exclude the GUI process from
the helper's stop operation.

## Developer trace flow

A local CLI attaches to the running application through its private Unix socket.
The collector's lifetime enables the trace layer and causes the sharing loop to
request `trace-v1` events from every connected, capable peer. Each peer keeps a
lease for the authenticated requesting link; dropping it disables that request.
Optional capabilities are carried in authenticated Noise handshake payloads,
independently of the required sharing protocol.

The tracing layer bounds formatted event fields and enqueues records without
network or disk I/O. Event-tap decisions use a separate copy-only queue drained by
the sharing task. A lower-priority sender queue and single outstanding-frame
credit prevent trace streams from accumulating ahead of input. The collector labels
remote records with the channel key and streams them to the CLI. Overflow drops
diagnostics and increments explicit loss counters. The native application also
observes foreground-application changes without changing focus.
