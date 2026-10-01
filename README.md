# Daisy

Daisy shares one keyboard and trackpad across the macOS systems on your desk. Move the pointer past the edge of one screen and it continues onto the next, with typing, clicks and trackpad swipes following it.

Daisy needs no account and no cloud service. Connections run directly over your local network, and because both ends run macOS, input arrives exactly as it was entered.

## Current Status

Daisy is in beta. It supports:

- Pointer movement, clicks, drags and typing across a shared screen edge
- Scrolling, with trackpad momentum, and swipes for Spaces, Mission Control and app windows
- Clipboard sharing for text, rich text and images
- Automatic reconnection after sleep or a network change
- Connection status that names the peer and shows its round-trip latency
- Finding paired peers on the local network with Bonjour
- Pairing with a one-time code, managed from the menu bar
- Immediate return of control with Control-Option-Command-Escape

Planned: layouts that chain more than two screens.

Daisy requires Apple silicon and macOS 26 or later. Passing trackpad swipes through requires macOS 27 on the system whose trackpad you use.

## Getting Started

1. Download the DMG from the latest release on [Releases](https://github.com/misfitdev/daisy/releases), open it and drag **Daisy** to Applications. Install it on both systems.
2. Open Daisy. It walks through allowing Accessibility and Input Monitoring, which macOS requires before Daisy can read and send input.
3. Click **Pair a New Peer** on both systems. Nearby systems find each other automatically. Enter the six-digit code shown on one into the other.
   - Once connected, drag the peer's screen to the side where it sits, as in Displays. Daisy agrees on one arrangement for the pair.
   - For networks without Bonjour, click **Advanced…** and enter the peer's address on one system, leaving it empty on the other.
4. Click **Start Sharing** on both, then move the pointer through the chosen edge. The menu bar flower shows a yellow center while connected.

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
