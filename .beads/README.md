# Beads in Daisy

Daisy uses [Beads](https://github.com/gastownhall/beads) for its durable roadmap
and implementation history. The shared source of truth is a private DoltHub
database. Each clone has a local embedded Dolt database at
`.beads/embeddeddolt`; issue updates are pushed to DoltHub and are not committed
to the Daisy Git repository. The JSONL export is local only and ignored by Git.

## Fresh clone

Configure DoltHub authentication before connecting. Beads 1.3 adopts Git's
`origin` as a Dolt remote during initialization, so replace that mapping with
Daisy's DoltHub remote:

```bash
bd init --non-interactive --skip-agents
bd dolt remote remove origin
bd dolt remote add origin <private-dolthub-remote>
bd dolt pull --remote origin
bd config set dolt.auto-push true
bd config set export.auto false
```

Replace `<private-dolthub-remote>` with the private remote locator configured
for the clone. The Dolt remote named `origin` is separate from Daisy's Git
remote. Pull before working in another clone; Beads auto-pushes issue changes
but does not auto-pull changes from other clones. Avoid concurrent edits from
multiple clones; pull and resolve any Dolt conflicts before continuing.

Confirm the workspace:

```bash
bd where
bd stats
bd ready
bd dolt remote list
```

## Normal workflow

```bash
bd show <id>
bd update <id> --claim
bd update <id> --notes="What changed and how it was verified"
bd close <id> --reason="Completed and verified"
```

Use Beads for shared tasks, blockers, follow-up work and knowledge that must
survive a session. Do not create Markdown TODO lists as a second roadmap.

## Health checks

```bash
bd lint
bd orphans
bd stale
bd doctor --check=conventions
bd doctor --check=pollution
```

`bd doctor` without a specific supported check is unavailable in embedded mode.
`bd preflight --check` in Beads 1.3.0 assumes Beads' own Go repository and is
not the Daisy quality gate; use `just check` plus the security commands in
[CONTRIBUTING.md](../CONTRIBUTING.md).
