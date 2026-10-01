# Daisy

Daisy shares one keyboard and trackpad across the macOS systems on your desk. Move the pointer past the edge of one screen and it continues onto the next, with typing, clicks and trackpad swipes following it.

Daisy needs no account and no cloud service. Connections run directly over your local network, and because both ends run macOS, input arrives exactly as it was entered.

## Current Status

Daisy is in beta. It supports:

- Groups of up to eight systems: the pointer moves onto whichever screen it reaches, with typing, clicks and drags following it
- Every display of every system arranged as in Displays, by dragging
- Scrolling, with trackpad momentum, and swipes for Spaces, Mission Control and app windows
- Clipboard sharing for text, rich text and images
- Automatic reconnection after sleep or a network change
- Connection status that names each member and shows its round-trip latency, plus `daisy stats` percentiles
- Finding paired peers on the local network with Bonjour
- Pairing with a one-time code; a system paired with any member joins the whole group
- Immediate return of control with Control-Option-Command-Escape

Daisy requires Apple silicon and macOS 26 or later. Passing trackpad swipes through requires macOS 27 on the system whose trackpad you use.

## Getting Started

1. Download the DMG from the latest release on [Releases](https://github.com/misfitdev/daisy/releases), open it and drag **Daisy** to Applications. Install it on every system.
2. Open Daisy. It walks through allowing Accessibility and Input Monitoring, which macOS requires before Daisy can read and send input.
3. Click **Pair a New Peer** on both systems. Nearby systems find each other automatically. Enter the six-digit code shown on one into the other. To add another system, pair it with any one of them.
   - Once connected, drag each system's displays to where they sit, as in Displays. Every member keeps the same arrangement.
   - For networks without Bonjour, click **Advanced…** and enter the peer's address on one system, leaving it empty on the other.
4. Click **Start Sharing** on each, then move the pointer off a screen toward another system's. The menu bar flower shows a yellow center while connected.

After pairing, they recognize each other; a new code is needed only when trust ends.

## Trust

Each system asks how long to trust a peer when they pair: until it goes unused for a number of hours or days (four days by default; regular use keeps it active), for this session only, or until you forget it. Change it any time in the **Peers** group; the new choice starts from that moment. When the two sides differ, the shorter one applies. After trust expires, pair again with a new code.

## Security

Only paired devices can connect, and pairing requires consent on both sides. Input, clipboard contents and pairing messages are encrypted in transit. The pairing code works once and is never sent over the network, so it cannot be captured by anyone listening.

The [security model](docs/security-model.md) describes the design in detail. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

Releases are built by GitHub Actions from tagged commits, signed with a Developer ID certificate and notarized by Apple. To confirm a download came from this repository:

```bash
gh attestation verify Daisy-0.1.2-macos-arm64.dmg --repo misfitdev/daisy
```

## Documentation

- [Usage](docs/usage.md): setup, operation, troubleshooting and the command line
- [Architecture](docs/architecture.md), [Protocol](docs/protocol.md) and [macOS internals](docs/macos.md)
- [Releasing](docs/releasing.md): building, signing and verifying releases

To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Daisy is available under the [Apache License 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.
