# Security policy

## Reporting a vulnerability

Report suspected vulnerabilities privately through [GitHub private vulnerability reporting](https://github.com/misfitdev/daisy/security/advisories/new). Do not disclose them in public issues, pull requests or discussions.

A report should include:

- A description of the vulnerability and the affected component.
- The affected version and the macOS version of each system involved.
- Steps to reproduce, including configuration and any proof-of-concept code.
- The assessed impact and the preconditions an attacker requires.

## Response and disclosure

Reports are acknowledged within seven days. Confirmed vulnerabilities are handled through coordinated disclosure: a GitHub security advisory is published once a fixed release is available, crediting the reporter unless they request anonymity.

## Supported versions

Security fixes are issued for the [latest release](https://github.com/misfitdev/daisy/releases/latest) only. Include your installed version when reporting a problem.

## Scope

In scope:

- Reading, altering or injecting input traffic from a network position.
- Completing pairing without the one-time code.
- Establishing a session as a peer that has not been paired.
- Disclosure of a system's private identity key.
- Weaknesses in release signing, notarization or build provenance.

Out of scope:

- Actions taken by a paired peer, which pairing authorizes by design.
- Attacks that require the user's account or root on either system.

[docs/security-model.md](docs/security-model.md) documents the security design.
