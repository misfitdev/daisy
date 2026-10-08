# Contributing

Build, test and submit a change to Daisy. Hardware behavior needs evidence from hardware; unit tests cover decisions.

## Setup

You need an Apple silicon system running macOS 26 or later, plus [mise](https://mise.jdx.dev).

```bash
git clone https://github.com/misfitdev/daisy
cd daisy
mise install      # Rust, project commands, release notes and security scanners
just              # list the tasks
```

## Build and run a local app

Running the app or CLI with a persistent device identity requires a signed `Daisy.app` with an embedded provisioning profile for `dev.misfit.daisy`. Set `DAISY_PROVISIONING_PROFILE` to the profile’s path. `just app` defaults to a Developer ID Application certificate; set `DAISY_SIGN_IDENTITY` to select another certificate authorized by the profile. See [release setup](docs/releasing.md#one-time-setup) for the profile requirements. Unlock this system before creating or rotating its device key.

```bash
export DAISY_PROVISIONING_PROFILE=/path/to/Daisy.provisioningprofile
just app
```

`just check` uses software signing keys in unit tests and needs no signing credentials.

Without `DAISY_PROVISIONING_PROFILE`, `just app` signs ad hoc. An ad hoc bundle exercises the interface, but cannot create the persistent Keychain device identity.

To try the interface without permissions or pairing, run `just dev`. It starts an ad hoc `Daisy.app` on a throwaway data folder with sample peers, so buttons such as **Remember until…** and the trash can change only that copy. Close Set Up Daisy and choose **Open Daisy** from the menu bar.

## Check a change

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

## Check documentation and website changes

The task index, setup, trust and troubleshooting pages live in `docs/`. The website renders the same Markdown. Edit the canonical page rather than adding a second copy to the site.

```bash
npm --prefix site ci
npm --prefix site test
npm --prefix site run build
```

The build rejects broken local documentation links, image paths and section anchors. Keep GitHub-relative links in Markdown; the site rewrites them to its routes. Website-only visual changes also need inspection at wide and narrow sizes.

## Testing on hardware

Tests cover decisions: crossing edges, releasing held keys, recognizing swipes, pairing and encryption. They cannot prove that macOS acts on captured or posted events, so changes to input, the pointer or swipes need a run on two systems:

1. Run `just app permissions --request` on each system, then grant Daisy Accessibility and Input Monitoring in System Settings. `just app` uses the certificate and provisioning profile configured above; keeping the signed app identity stable preserves permissions across rebuilds.
2. On one system, run `just app listen --pair`.
3. On the peer, build the same commit and run `just app connect <first-system>.local --pair --side right`, using the side where the first system's screen sits, then type the displayed code.
4. Prefix either command with `RUST_LOG=daisy=debug` to see each side's decisions.

Say in the pull request which macOS versions you tested and which system you were using at each step. Control-Option-Command-Escape takes control back if anything goes wrong.

## Developer trace collection

Run a trace-capable Daisy build on each system you want to inspect. Leave sharing
running, then attach from a terminal on **one** system:

```bash
/Applications/Daisy.app/Contents/MacOS/daisy trace --output "$HOME/Desktop/daisy-trace.ndjson"
```

Use `daisy trace` if the CLI is already on your path. With a custom data folder,
pass the same `--home` used by the running instance. The output path must be new;
the command creates a file readable only by your account. Press **Control-C** to
stop. Closing the terminal or losing the requesting connection also disables its
trace request. Another connected collector can keep its own request active.

The command dynamically enables Daisy's trace events on this system and every
supported, connected peer. It collects them through the existing authenticated,
encrypted mesh connections; no restart or `RUST_LOG` setting is needed. Peers that
lack trace support keep sharing, and the collector records an update notice for
them. Their internal logs are unavailable until they run a trace-capable build.
Normal stderr logging retains its existing filter.

Each NDJSON line identifies its source by the authenticated system key and contains
the build, event level, target, fields, source wall-clock time (`unix_ms`), monotonic
elapsed time, and sequence. Control generations correlate ownership decisions
across systems even when their clocks differ. `dropped_before` counts source or
link-queue losses; `collector_dropped_before` counts losses at the collector.
Queues and records are bounded, and input and heartbeats take priority over traces.

For unexpected control returns, look for `sharing transition`, `control claim
decision`, `local capture decision`, `foreground application changed`, `display
activity decision`, `display user activity refreshed`, and heartbeat failures.
Capture decisions distinguish physical-input claims, the emergency-return shortcut,
and queue or lock recovery. Event-source process IDs and event types help identify
unexpected generated input. Foreground changes are observations, not proof of what
caused a control claim. The diagnostics do not record typed keycodes, input payloads,
or clipboard contents. Review the file before sharing it: it contains system keys,
process IDs, connection details, and error messages.


### Investigate an unexpected control transition

Start [developer trace collection](#developer-trace-collection) on one
system while the mesh is connected. Reproduce the jump, note the time and which
system you were using, then stop collection with Control-C. Include both Daisy
versions if the behavior began after an update, and whether you pressed
Control-Option-Command-Escape.

The combined file distinguishes edge entry/exit, physical-input claims, emergency
returns, rejected or stale claims, local capture recovery, foreground-application
changes, display-activity refreshes, and link failures. A display refresh near a
jump is a lead to investigate; timing alone does not establish a cause.

### Implementation and validation

Optional `trace-v1` support is exchanged in the authenticated Noise handshake and
bound into the Secure Enclave proof. The required session protocol stays unchanged.
Fixed-byte tests pin the optional `trace-v1` encoding as well as its message tags.
Changing that contract requires a new capability name; keep the existing version
readable while peers still use it.
The local command uses a same-account Unix socket and does not open another sharing
instance. Each requesting link owns a trace lease; collector shutdown sends a
disable request, and connection loss releases the peer lease. Records are attributed
using channel keys, rather than any identity in a record.

Control-state snapshots are copied and their decision locks released before any
trace formatting. Capture treats lock contention as a recovery condition, so
logging must never extend those critical sections. A regression subscriber checks
that the capture decision lock is available during transition emission.

The trace layer formats bounded fields into a queue; the event tap enqueues only
copy-only facts and never formats logs or performs I/O. Separate low-priority
queues and one outstanding trace-frame credit preserve input and heartbeat
priority. Acknowledgments must match the outstanding sequence. Late records after
collection stops are acknowledged and discarded. See the [wire contract](docs/protocol.md#developer-tracing)
and [trust boundary](docs/security-model.md#developer-diagnostics).

Run `just check` to cover dynamic activation, old/new capability combinations,
private local attachment, collector shutdown, source attribution, queue saturation,
loss accounting, and stale acknowledgments. `tests/developer_trace.rs` exercises
activation and collection over encrypted in-memory connections with no native
capture or injection. Foreground notifications and captured event origins still
need observations on the affected systems; unit tests do not establish the cause
of an intermittent focus or control jump.

## Code

[AGENTS.md](AGENTS.md) contains the rules that matter most for people and coding agents:

- Wire enums are append-only: never reorder or remove a variant.
- The event-tap callback must never block, start a process or panic.
- Direction conventions are pinned to values recorded on hardware, not to each other.
- A regression test for a fix must fail when the fix is removed.

Design notes live in [docs/](docs/): [usage](docs/usage.md), [architecture](docs/architecture.md), [protocol](docs/protocol.md), [security model](docs/security-model.md), [macOS internals](docs/macos.md) and [releasing](docs/releasing.md).

Document user-visible behavior in the [User guide](docs/usage.md) and relevant website copy. When hardware verification changes a feature's status, update its Bead and those documents in the same change.

## Commits and pull requests

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org). `feat`, `fix`, `perf` and `docs` subjects become release notes, so write them for someone installing the release: “Forward left and right swipes the right way round,” not “Flip sign in synthetic_sign.”

Explain why in the body; the diff shows what. Keep pull requests to one change with its test.

## Issues and security

Report bugs and ideas in GitHub issues. The maintainer's roadmap is tracked with [Beads](https://github.com/gastownhall/beads) and exported to `.beads/issues.jsonl`.

Report security problems privately, never in a public issue; see [SECURITY.md](SECURITY.md).

## License

By contributing, you agree that your contributions are licensed under the same terms as the project, MIT or Apache-2.0 at the user's option, without additional terms.
