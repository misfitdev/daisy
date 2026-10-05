# Daisy

Daisy shares one keyboard and trackpad across the macOS systems on your desk. Move the pointer past the edge of one screen and it continues onto the next, with typing, clicks and trackpad swipes following it.

Daisy needs no account and no cloud service. Connections run directly over your local network, and because both ends run macOS, input arrives exactly as it was entered.

## Current Status

Daisy supports:

- Groups of up to eight systems: the pointer moves onto whichever screen it reaches, with typing, clicks and drags following it
- Every screen of every system arranged by dragging
- Scrolling, with trackpad momentum, and swipes for Spaces, Mission Control and app windows
- Clipboard sharing for text, rich text and images
- Activity on the system in control keeps unlocked peer displays awake; normal display sleep and lock timers resume when input stops
- Automatic reconnection after sleep or a network change
- Connection status that names each member and shows its round-trip latency, plus `daisy stats` percentiles
- Finding paired peers on the local network with Bonjour
- Pairing with a one-time code; a system paired with any member joins the whole group
- Control-Option-Command-Escape returns control and brings the pointer to the middle of the main display

Daisy requires Apple silicon and macOS 26 or later. Trackpad swipes work from macOS 26 and later.

## Updating to 0.6.0

Update every member of the group. Daisy 0.6.0 uses protocol 6 and cannot connect to 0.5.0 peers. Existing device identities and peer trust are retained.

## Getting Started

1. Download the DMG from the latest release on [Releases](https://github.com/misfitdev/daisy/releases), open it and drag **Daisy** to Applications. Install it on every system.
2. Open Daisy. It walks through allowing Accessibility and Input Monitoring, which macOS requires before Daisy can read and send input.
3. Click **Start Sharing** on both systems. They find each other automatically, and one shows a six-digit code. Type it on the other and click **Connect**.
   - Once connected, drag each system's screens to where they sit. Every member keeps the same arrangement.
   - To add another system, click **Start Sharing** on it, then choose **Add a System** from the menu bar flower on any member and type the code the new system shows.
   - For networks without Bonjour, click **+** beside **Peers**, enter the peer’s local name or IP address, then click **Save and Connect**. The other system must be sharing.
4. Move the pointer off a screen toward another system's. The menu bar flower shows a yellow center while connected.

After pairing, they recognize each other; a new code is needed only when trust ends.

## Trust

A new peer is trusted until it goes unused for four days; regular use keeps it active. Click its trust in the **Peers** group to choose a number of hours or days, this session only, or until you forget it; the new choice starts from that moment. When the two sides differ, the shorter one applies. After trust expires, pair again with a new code.

## Security

Only paired systems can connect. A system accepts a new one only while it has no peers, during **Add a System**, or with **Always discoverable** turned on. Input, clipboard contents and pairing messages are encrypted in transit. The pairing code works once and is never sent over the network, so it cannot be captured by anyone listening.

The [security model](docs/security-model.md) describes the design in detail. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

Releases are built by GitHub Actions from tagged commits, signed with a Developer ID certificate and notarized by Apple. To confirm a download came from this repository:

```bash
gh attestation verify Daisy-0.6.0-macos-arm64.dmg --repo misfitdev/daisy
```

## Documentation

- [Usage](docs/usage.md): setup, operation, troubleshooting and the command line
- [Architecture](docs/architecture.md), [Protocol](docs/protocol.md) and [macOS internals](docs/macos.md)
- [Releasing](docs/releasing.md): building, signing and verifying releases

To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Daisy is available under the [Apache License 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.
