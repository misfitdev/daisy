<a href="https://misfitdev.github.io/daisy/">
  <img src="https://misfitdev.github.io/daisy/brand/daisy-lockup.svg" alt="Daisy" width="240">
</a>

Daisy shares your keyboard, mouse, trackpad and clipboard across macOS systems on the same network. Move the pointer from one screen to another and keep typing, clicking and swiping.

**Requires Apple silicon and macOS 26 or later.**

**[Download Daisy](https://github.com/misfitdev/daisy/releases/latest)** · [Homebrew installation](docs/usage.md#homebrew) · [Website](https://misfitdev.github.io/daisy/)

## Features

- Arrange up to eight systems, with every display placed to match your desk.
- Use trackpad swipes for Spaces, Mission Control and app windows.
- Copy and paste text, rich text and images between systems.

## Getting started

1. **Install.** Download the DMG and drag Daisy to Applications, or [install with `brew`](docs/usage.md#homebrew). Do this on each system.
2. **Start.** Open Daisy, follow the permission prompts and click **Start Sharing** on both systems.
3. **Pair.** Enter the code shown on one system into the other and click **Connect**.

Move the pointer past a screen edge toward another system. Daisy remembers paired systems and reconnects automatically.

For an existing installation, the [staged update command](docs/usage.md#install-a-staged-update)
verifies a newer compatible release before restarting and retains the previous
copy until startup succeeds.

## Security

Connections stay on your local network and are encrypted between paired systems. Daisy needs no account or cloud service.

Read the [security model](docs/security-model.md) or [report a vulnerability privately](SECURITY.md).

## Documentation

- [Task index](docs/README.md) · [User guide](docs/usage.md): setup, settings, troubleshooting and the command line.
- [Architecture](docs/architecture.md), [protocol](docs/protocol.md) and [macOS internals](docs/macos.md).
- [Contributing](CONTRIBUTING.md) and [release guide](docs/releasing.md): building, signing and verifying releases.

## License

Daisy is available under the [Apache License 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT), at your option.
