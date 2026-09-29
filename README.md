# Daisy

Daisy shares one keyboard and trackpad across the Macs on your desk. Move the pointer past the edge of one screen and it continues onto the next, with typing, clicks and trackpad swipes following it.

Daisy needs no account and no cloud service. Connections run directly over your local network, and because both ends run macOS, input arrives exactly as it was entered.

## Current Status

Daisy is in beta. It supports:

- Pointer movement, clicks, drags and typing across a shared screen edge
- Scrolling, and swipes for Spaces, Mission Control and app windows
- Clipboard sharing for text, rich text and images
- Automatic reconnection after sleep or a network change
- Pairing with a one-time code, managed from the menu bar
- Immediate return of control with Control-Option-Command-Escape

Planned: automatic discovery on the local network, and layouts that chain more than two screens.

Daisy requires Apple silicon and macOS 26 or later. Passing trackpad swipes through requires macOS 27 on the Host.

## Getting Started

1. Download the latest release from [Releases](https://github.com/misfitdev/daisy/releases), unzip it and move **Daisy.app** to Applications. Install it on both Macs.
2. Open Daisy from the menu bar and grant Accessibility and Input Monitoring when prompted. macOS requires both before Daisy can read and send input.
3. Pair them:
   - On one, choose **Wait for a peer**. On the other, choose **Connect by Address** and enter the first one's network name, such as `studio.local`.
   - Choose **Host** where you will type and use the trackpad, and **Guest** on the other. On the Host, choose the edge where the Guest sits.
   - Click **Pair a New Peer** on both. Enter the six-digit code shown on one into the other.
4. Click **Start Sharing** on both, then move the pointer through the chosen edge. The menu bar flower shows a yellow center while connected.

After pairing, they recognize each other; a new code is needed only when trust ends.

## Trust

A pairing stays trusted until four days pass without a connection; regular use keeps it active. Each side can choose a different duration under **Paired Peers**: 30 days, the current session only, or until removed. When the two sides differ, the shorter duration applies. After trust expires, pair again with a new code.

## Security

Only paired devices can connect, and pairing requires consent on both sides. Input, clipboard contents and pairing messages are encrypted in transit. The pairing code works once and is never sent over the network, so it cannot be captured by anyone listening.

The [security model](docs/security-model.md) describes the design in detail. Report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

Releases are built by GitHub Actions from tagged commits, signed with a Developer ID certificate and notarized by Apple. To confirm a download came from this repository:

```bash
gh attestation verify Daisy-0.1.1-macos-arm64.zip --repo misfitdev/daisy
```

## Documentation

- [Usage](docs/usage.md): setup, operation, troubleshooting and the command line
- [Architecture](docs/architecture.md), [Protocol](docs/protocol.md) and [macOS internals](docs/macos.md)
- [Releasing](docs/releasing.md): building, signing and verifying releases

To contribute, see [CONTRIBUTING.md](CONTRIBUTING.md).

## License

Daisy is available under the [Apache License 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.
