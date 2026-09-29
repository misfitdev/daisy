# Daisy

Share one keyboard and mouse between Macs on your network, trackpad swipes included, with no Apple ID, iCloud or account of any kind.

Daisy is Mac-to-Mac only by design. Both ends speak macOS natively, so keys, clicks and swipes pass through as themselves instead of being translated through another operating system.

## Status

**Beta.** The native menu-bar app handles setup, pairing, permissions and daily control. Automatic discovery is still planned.

| Capability | Status |
|---|---|
| Native menu-bar setup, pairing, trust and connection control | Works |
| Pairing, encrypted reconnects, per-Mac trust policies and forced revocation | Works |
| Keyboard, mouse, clicks, double-clicks and drags across a shared screen edge | Works |
| Trackpad and mouse-wheel scrolling | Works |
| Spaces, Mission Control and application-window swipes | Verified end to end |
| Shake to locate on the controlled Mac | Verified on macOS 27; macOS 26 visual verification remains |
| Control-Option-Command-Escape recovery chord | Works |
| Clipboard sharing of text, rich text and images when control crosses | Implemented; two-Mac verification remains |
| Reconnecting after sleep, wake and network changes | Implemented; two-Mac verification remains |
| Automatic discovery | Planned |
| Positioned layouts and chained connections among many Macs | Planned |

Requires macOS 26 or later on Apple silicon. Reading trackpad swipes requires macOS 27 on the driving Mac. See the [usage guide](docs/usage.md) for setup, recovery and version-specific behavior. The complete roadmap is in `.beads/issues.jsonl`.

## Install

Download `Daisy-<version>-macos-arm64.zip` from [releases](https://github.com/misfitdev/daisy/releases), unzip it and move `Daisy.app` to `/Applications`. Releases are signed with Developer ID and notarized by Apple.

Open `Daisy.app`. Daisy walks you through Accessibility and Input Monitoring; macOS grants both permissions to the signed app, not Terminal.

## Pair two Macs

1. On one Mac, choose **Wait for a peer**. Choose **Host** if its keyboard and trackpad will be shared, or **Guest** if it will receive input, then click **Pair a New Peer**.
2. On the other Mac, choose **Connect by Address**, enter the first Mac's local name such as `studio.local`, and choose the other role. Exactly one Mac is the Host. Then click **Pair a New Peer**.
3. On the Host, choose the screen edge that leads to the Guest.
4. Enter the six-digit code shown by the waiting Mac on the connecting Mac. The code is used directly by secure pairing; there is nothing to compare by eye.

After pairing, choose **Start Sharing** on both Macs. Push through the configured screen edge to move control, and push back out of the far edge to return. The menu-bar flower has a yellow center while connected and a gray center otherwise.

The [usage guide](docs/usage.md) covers roles, connection direction, permissions, trust, the optional CLI, troubleshooting, data locations and removal.

### How long trust lasts

Each Mac decides how long it trusts the other. Both must still trust each other to connect, so the stricter choice wins. Choose a policy while pairing or change it later from **Paired Peers** in the menu:

| Policy | Trusted until |
|---|---|
| Until 4 days inactive (default) | 96 hours pass without a connection; staying connected renews it |
| 30 days | 30 days after pairing, however often the Macs connect |
| This session | The session ends; after an accidental drop, but not a deliberate disconnect, it may reconnect within 60 seconds |
| Until I forget | You forget it |

When trust ends, the Mac is forgotten and must pair again with a new code.

## Verify a download

Every release is built by GitHub Actions from a tagged commit, with SLSA Build Level 3 provenance and a GitHub artifact attestation. Either check proves the zip came from this repository's release workflow:

```bash
gh attestation verify Daisy-0.1.1-macos-arm64.zip --repo misfitdev/daisy

slsa-verifier verify-artifact Daisy-0.1.1-macos-arm64.zip \
  --provenance-path Daisy-0.1.1-macos-arm64.zip.intoto.jsonl \
  --source-uri github.com/misfitdev/daisy --source-tag v0.1.1
```

## Security

Connections use encrypted Noise sessions. Unknown Macs can pair only while both sides explicitly allow pairing. The six-digit code is SPAKE2 input, not a value to compare visually.

The [security model](docs/security-model.md) covers trust and failure containment. Report vulnerabilities as described in [SECURITY.md](SECURITY.md).

## Documentation

- [Usage](docs/usage.md): installation, pairing, operation and troubleshooting
- [Architecture](docs/architecture.md): roles, layers and threads
- [Protocol](docs/protocol.md): wire format and session flow
- [Security model](docs/security-model.md): what is protected, how and what is not
- [macOS internals](docs/macos.md): permissions, event tap, pointer pinning and swipes
- [Releasing](docs/releasing.md): signing, notarization and release verification

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Coding agents should start with [AGENTS.md](AGENTS.md).

## License

Licensed under either the [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
