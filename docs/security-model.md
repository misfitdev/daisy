# Security model

What Daisy protects, how it does so, and what it does not protect. To report a vulnerability, see [SECURITY.md](../SECURITY.md).

## What is at stake

A following Mac hands another Mac its keyboard and mouse. A paired peer can type and click anything the user could. Trust between two Macs is therefore total; the design's job is to ensure that only the Mac explicitly paired can act in that role.

## Threat model

In scope:

- **Anyone on the network**, passive or active: reading, altering, replaying or injecting traffic, or sitting in the middle of a connection.
- **A Mac that was not paired** attempting to connect.

Out of scope:

- A Mac that was deliberately paired. Trusted input is the capability pairing grants.
- Someone with the user's account or root on either Mac. They can read the key or grant themselves the same permissions.

## Connections

Every connection uses a Noise `XX` handshake with X25519, ChaCha20-Poly1305 and BLAKE2s, through a library and without a network extension or root.

After the handshake, every message is encrypted and authenticated with a per-direction nonce. Altered, replayed or reordered frames fail authentication. The prologue binds the protocol version into the handshake.

## Pairing

The handshake tells each Mac the other's long-term public key, but not whether it belongs to the intended Mac. Pairing settles that once:

1. Both Macs must opt in with **Pair a New Peer** (or `--pair` in the CLI). A Mac not opted in refuses an unknown key without showing a code.
2. The listening Mac shows a random six-digit code; the user types it on the connecting Mac.
3. Both run SPAKE2 over the Ed25519 group, keyed by that code.
4. Each proves it derived the same key with HMAC-SHA256 over the Noise handshake hash, labelled by role. The role label stops reflection; the handshake hash binds the proof to this connection.
5. Only after both proofs verify does each Mac pin the other's public key.

An attacker in the middle has a different handshake with each Mac and does not know the code, so it gets one guess per attempt: a one-in-a-million chance. Comparing a short code by eye would not be safe because an attacker could grind keys until screens matched; the code is only ever PAKE input.

Pairing mode accepts at most five unknown-key attempts in a ten-minute window and closes after the first successful pairing. Noise handshakes time out after 15 seconds; trust negotiation or code entry times out after two minutes. Stopping and choosing **Pair a New Peer** again, or restarting `listen --pair`, deliberately opens a new window.

## How long trust lasts

Trust is not forever by default. Each Mac keeps its own policy for every Mac it trusts:

- **Idle**, the default: trust ends after 96 hours without an authenticated session. A running session renews it.
- **A deadline**: N days from pairing. Connecting does not extend it, and a session still running at the deadline ends.
- **Once**: trust ends with the session. Only after an accidental drop—silence or network error, not deliberate close—may the same key reconnect without a code, for at most 60 seconds.
- **Forever**: trust lasts until explicitly forgotten. It must be chosen and is always displayed as such.

Each Mac enforces its own policy on its own clock. A session starts only while both Macs trust each other, so neither can extend the other's trust by claiming that it still trusts.

Expired trust is removed from the peer file. The menu-bar app can forget a peer and aborts its active session task immediately; the peer-file watcher remains the backstop for changes made by another process and reacts within one second. The optional CLI provides `daisy forget` and `daisy rotate-key` for bulk revocation and identity rotation.

## Reconnecting

The Mac that connects keeps a session alive by connecting again after a drop, sleep or network change. Every attempt runs the full Noise handshake and trust check, so an expired or forgotten Mac is refused exactly as on first contact. A reconnect is never allowed to pair: pairing is offered only until the first session starts. If the key that answers is not the key of the Mac it was connected to, Daisy stops instead of retrying. Only network failures are retried (refused, unreachable, reset, silent or timed out); trust, key, protocol and setup failures stop.

## Failure containment

Input capture runs in the macOS event-tap callback, where blocking would lag the entire Mac. Its handoff queue is bounded and uses nonblocking sends. If the queue fills or callback state is contended, Daisy passes the current event through locally, reclaims local routing when possible, signals overload without taking another blocking lock and terminates the session.

Network reads and writes run in dedicated tasks behind bounded queues. Trust revocation, local overload and the three-second silence deadline remain selectable even when a peer stops reading. When the driving input channel closes, accepted messages have at most one second to flush; a blocked flush returns an error instead of reporting clean shutdown.

The following Mac releases every held key and button whenever replay ends. The driving Mac reclaims local control when its capture path overloads or the session ends.

## Clipboard

When clipboard sharing is on, a Mac sends its clipboard to the paired Mac each time control crosses: plain text, rich text and images, up to 4 MB of text and 32 MB of image. It travels inside the encrypted session like input and is written straight to the receiving Mac's pasteboard. A paired Mac can therefore read whatever was on the clipboard at the moment control crossed, which is within the trust pairing already grants. Either Mac can turn sharing off; a Mac with it off neither sends nor writes. The receiver discards any item larger than its limit or longer than it declared, and never writes a partial item.

Items marked concealed or transient by their source app stay on the system
where they were copied. This includes clipboard entries from password managers
that use the standard macOS markers.

## Keys at rest

Each Mac's long-term private key is `~/Library/Application Support/daisy/identity`, readable only by the user (`0600` in a `0700` directory). Daisy refuses to load a key file readable by other users.

Paired public keys and trust policies live in `peers.toml` beside it. Every update locks, rereads and rewrites the only copy, preventing a running process from resurrecting a peer forgotten by another command.

## Release integrity

Releases are built only by the release workflow from a tagged commit, signed with Developer ID and notarized by Apple. Each zip carries SLSA Build Level 3 provenance and a GitHub artifact attestation. [releasing.md](releasing.md) explains how to verify both provenance and notarization.
