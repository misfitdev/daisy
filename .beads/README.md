# Beads in Daisy

Daisy uses [Beads](https://github.com/gastownhall/beads) for its durable roadmap and implementation history.

## Source of truth

The active issue database is the local embedded Dolt database at `.beads/embeddeddolt`. This repository deliberately has no Dolt remote.

`.beads/issues.jsonl` is the Git-tracked passive export. It lets a fresh clone load the roadmap, but merely checking out the file does not import it.

## Fresh clone

```bash
bd init --non-interactive --skip-agents
bd dolt remote remove origin
bd import -i .beads/issues.jsonl
bd dolt remote list
```

Beads 1.3 automatically maps a Git remote named `origin` to a Dolt remote
during `bd init`. Daisy intentionally uses only the committed JSONL export,
so remove that mapping before doing issue work. The final command must report no
configured remotes. Do not run `bd dolt push` or `bd dolt pull` for this project.

Confirm the workspace:

```bash
bd where
bd stats
bd ready
```

## Normal workflow

```bash
bd show <id>
bd update <id> --claim
bd update <id> --notes="What changed and how it was verified"
bd close <id> --reason="Completed and verified"
```

Use Beads for shared tasks, blockers, follow-up work and knowledge that must survive a session. Do not create Markdown TODO lists as a second roadmap.

## Health checks

```bash
bd lint
bd orphans
bd stale
bd doctor --check=conventions
bd doctor --check=pollution
```

`bd doctor` without a specific supported check is unavailable in embedded mode. `bd preflight --check` in Beads 1.3.0 assumes Beads' own Go repository and is not the Daisy quality gate; use `just check` plus the security commands in [CONTRIBUTING.md](../CONTRIBUTING.md).

## Export parity

To verify that the passive export exactly matches the database without changing either:

```bash
diff -u \
  <(bd --readonly export | jq -sS 'sort_by(.id)') \
  <(jq -sS 'sort_by(.id)' .beads/issues.jsonl)
```

An empty diff means the export is current. Beads writes the export during normal issue mutations; run `bd export -o .beads/issues.jsonl` when a manual refresh is required.
