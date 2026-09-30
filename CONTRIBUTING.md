# Contributing

Thanks for helping. Daisy is small and opinionated: Mac-to-Mac only, no accounts, and no claim about hardware behavior without hardware evidence.

## Setup

You need an Apple silicon system running macOS 26 or later, plus [mise](https://mise.jdx.dev).

```bash
git clone https://github.com/misfitdev/daisy
cd daisy
mise install      # Rust, project commands, release notes and security scanners
just              # list the tasks
```

## The gate

`just check` runs clippy with warnings as errors, `rustfmt --check` and the tests. It must pass before a change is committed, and CI runs the same gate on macOS 26.

## Security review

Mise pins the security tools used for dependency and workflow review. Run them when dependencies, trust boundaries, session handling or release automation change:

```bash
mise exec -- cargo audit
mise exec -- cargo deny check
mise exec -- reachsec check --path .
mise exec -- zizmor .github
mise exec -- actionlint
```

`cargo deny` may report allowed duplicate-version warnings; advisories, license failures, forbidden sources and workflow findings must be resolved or narrowly documented. ReachSec has no upstream `--version` flag, so `mise ls --current` is the version record.

## Testing on hardware

Tests cover decisions: crossing edges, releasing held keys, recognizing swipes, pairing and encryption. They cannot prove that macOS acts on captured or posted events, so changes to input, the pointer or swipes need a run on two systems:

1. Run `just app permissions --request` on each system, then grant Daisy Accessibility and Input Monitoring in System Settings. `just app` signs with your Apple Development certificate, which keeps permissions across rebuilds.
2. On one system, run `just app listen --pair`.
3. On the peer, build the same commit and run `just app connect <first-system>.local --pair --side right`, using the side where the first system's screen sits, then type the displayed code.
4. Prefix either command with `RUST_LOG=daisy=debug` to see each side's decisions.

Say in the pull request which macOS versions you tested and which system you were using at each step. Control-Option-Command-Escape takes control back if anything goes wrong.

## Code

[AGENTS.md](AGENTS.md) contains the rules that matter most for people and coding agents:

- Wire enums are append-only: never reorder or remove a variant.
- The event-tap callback must never block, start a process or panic.
- Direction conventions are pinned to values recorded on hardware, not to each other.
- A regression test for a fix must fail when the fix is removed.

Design notes live in [docs/](docs/): [usage](docs/usage.md), [architecture](docs/architecture.md), [protocol](docs/protocol.md), [security model](docs/security-model.md), [macOS internals](docs/macos.md) and [releasing](docs/releasing.md).

User-visible behavior belongs in the usage guide and README status table. When hardware verification changes a feature's status, update its Bead and those documents in the same change.

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org). `feat` and `fix` subjects become release notes, so write them for someone installing the release: “Forward left and right swipes the right way round,” not “Flip sign in synthetic_sign.”

Explain why in the body; the diff shows what. Keep pull requests to one change with its test.

## Issues and security

Report bugs and ideas in GitHub issues. The maintainer's roadmap is tracked with [Beads](https://github.com/gastownhall/beads) and exported to `.beads/issues.jsonl`.

Report security problems privately, never in a public issue; see [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the same terms as the project, MIT or Apache-2.0 at the user's option, without additional terms.
