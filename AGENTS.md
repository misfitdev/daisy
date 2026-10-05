# Agent Instructions

Daisy shares one system's keyboard, mouse and trackpad swipes with a peer on
the same network. It is Rust, macOS only, and uses private WindowServer
events for swipes. Read this before changing anything.

## Build and test

```bash
just              # list recipes
just check        # clippy -D warnings, rustfmt --check, cargo test
just app <args>   # build, sign and run Daisy.app
just screenshot   # redraw the website's window screenshot after UI changes
just package      # create the release zip and DMG; notarize when credentials are set
```

Running the app or CLI with a persistent device identity requires an embedded
provisioning profile for `dev.misfit.daisy`. Set `DAISY_PROVISIONING_PROFILE` to
its path and use a certificate it authorizes; `just app` defaults to Developer ID
Application. Unit tests use software keys and require no profile. Ad hoc bundles
cannot create a persistent Keychain device identity. See `docs/releasing.md`.

`just check` is the gate. It must exit 0 before anything is committed, and
its own exit status counts. Never pipe it through a command that can hide a
failure.

For dependency, trust-boundary or release-workflow changes, also run:

```bash
mise exec -- cargo audit
mise exec -- cargo deny check
mise exec -- reachsec check --path .
mise exec -- zizmor .github
mise exec -- actionlint
```

ReachSec has no upstream `--version` flag; use `mise ls --current` to record
its pinned revision.

Input capture, injection, cursor pinning and swipes only prove themselves on
hardware: two systems, each with Accessibility and Input Monitoring granted to
`Daisy.app`. Unit tests cover decisions, not whether macOS acts on the
events. Do not call hardware behavior verified unless it ran on hardware in
this session; record which system was used at each step and the macOS
version of each system.

## Architecture

- `src/protocol.rs` is the only wire-message definition.
  `docs/protocol.md` explains it; code wins if they disagree.
- `src/device.rs` defines device public keys, signature verification and Keychain
  reference persistence. `src/macos/device.rs` performs Secure Enclave operations.
  Production signing keys must remain non-exportable; software signing is only
  for unit tests. Both connection paths must verify a device proof over the Noise
  handshake hash and sender role before trust negotiation. Pairing pins both the
  Noise key and device key; a matching Noise key never overrides a device mismatch.
- Pure decision modules contain no macOS calls and are unit tested directly:
  `input`, `swipe`, `shake`, `pairing`, `trust`, `session`, `control`,
  `latency`, `share`, `layout`, `install` and `setup`. Keep new decisions
  out of `src/app/` and the network loops so they can be tested this way;
  the pairing gate (`service::PairingGate`) and opener election
  (`discovery::pairing_opener`) are examples.
- `src/macos/` carries out decisions and nothing more. Undocumented
  WindowServer fields and event types live only in `src/macos/swipe.rs`;
  Mission Control shortcut IDs live only in `src/macos/shortcut.rs`.
- `docs/` contains usage, administration, architecture, protocol, security,
  macOS and release documentation.
- Settings an administrator can enforce live only in
  `src/macos/managed.rs`, read from the `dev.misfit.daisy` domain and honored
  only when a configuration profile forces them. Document every key in
  `docs/administration.md`.

## Conventions and patterns

- Wire enums (`protocol::Message`, `input::InputEvent`) are encoded by
  variant position. Append variants; never reorder or remove them.
- The Bonjour TXT record (`discovery::properties`) is read by every
  version on the network: `p` is 0 closed, 1 open with no peers, 2 open
  group member; `s` is when pairing opened. Add keys freely; change an
  existing key's meaning only with a new `VERSION` there.
- Short codes are only safe as PAKE input. Never ask people to compare a code
  by eye as a security check; an attacker can grind keys until codes match.
- The event-tap callback runs on every input event. It must never wait, perform
  I/O, start a process or panic. Cache anything it needs.
- Replaying a captured event with the sign it was captured with cannot catch a
  sign error because the mistake cancels out. Pin directions to hardware
  values, with date and macOS version.
- If something does not work, remove it. Do not keep it behind a guard, flag or
  documentation note.
- Every regression test must be shown to fail when its defect is reintroduced.
- Commit messages are Conventional Commits and become release notes. `feat`,
  `fix`, `perf` and `docs` are listed; `chore`, `ci`, `build`,
  `test`, `style` and `refactor` are hidden. Write subjects for someone
  installing the release. Do not add attribution trailers.
- Do not put surnames, email addresses or machine names in the repository.
- Do not put issue IDs in docs, code comments or site copy. Track status in
  Beads, not in prose.
- Never call a device a "Mac" in code, comments, UI or CLI text, docs or site
  copy. A remote device is a peer; the local device is this system or the
  local system. Whichever system is in use drives; there are no roles to
  name. Apple platform and product names are fine when that specific thing
  is meant: "macOS 27", "Apple silicon", "Mission Control". Describe the
  platform scope as "macOS-native" or "Mac-to-Mac".
- Describe Daisy on its own terms. Public documentation must not mention or
  compare Daisy with other products.

## Non-interactive shell commands

Use non-interactive flags for file operations so an alias adding `-i` cannot
hang: `cp -f`, `mv -f`, `rm -f`, `rm -rf`, `cp -rf`. Use
`-o BatchMode=yes` with `ssh` and `scp`, and
`HOMEBREW_NO_AUTO_UPDATE=1` with `brew`.

## Beads in this repository

Issues are committed to Git through the passive export
`.beads/issues.jsonl`. The active source of truth is the local embedded Dolt
database, and there is deliberately no Dolt remote.

On a fresh clone:

```bash
bd init --non-interactive --skip-agents
bd dolt remote remove origin
bd import -i .beads/issues.jsonl
bd dolt remote list
```

Beads 1.3 automatically adopts Git `origin` as a Dolt remote during `bd init`.
Remove it before importing; the final command must report no configured remotes.

<!-- BEGIN BEADS INTEGRATION v:1 profile:minimal hash:6cd5cc61 -->
## Beads issue tracker

Run `bd prime` for the full workflow.

```bash
bd ready
bd show <id>
bd update <id> --claim
bd close <id>
```

- Use Beads for all durable task tracking. Do not create Markdown TODO lists.
- Use `bd remember` for persistent project knowledge.
- Do not run Git commits, Git pushes or Dolt synchronization without explicit
  authorization.
- Before handoff, file remaining work, run relevant gates, update issue state,
  and report changed files, validation and blocked publication steps.
- This repository has no Dolt remote. The Git-tracked JSONL export is the
  cross-clone roadmap.
<!-- END BEADS INTEGRATION -->

<!-- BEGIN BEADS CODEX SETUP: generated by bd setup codex -->
## Codex Beads setup

Codex follows the Beads instructions above and uses
`.agents/skills/beads/SKILL.md`. Native hooks may load `bd prime`; run it
manually whenever tracker context is missing or stale.
<!-- END BEADS CODEX SETUP -->
