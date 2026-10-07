# Security model

What Daisy protects, how it does so, and what it does not protect. To report a vulnerability, see [SECURITY.md](../SECURITY.md).

## What is at stake

Each system lets every system in its group type and click on it. A member can type and click anything the user could. Trust between members is therefore total; the design's job is to ensure that only systems paired into the group, directly or through a member, can act in that role.

## Threat model

In scope:

- **Anyone on the network**, passive or active: reading, altering, replaying or injecting traffic, or sitting in the middle of a connection.
- **A system that was not paired** attempting to connect.

Out of scope:

- A system that was deliberately paired, and what it does as a member: trusted input is the capability pairing grants. A member can also introduce a new system to the group, or revoke one (see [Groups](#groups)), so a compromised member can admit or remove systems.
- Someone controlling the user’s account or root on a paired system. They can request signatures from that system’s Secure Enclave while running there.

## Connections

Every connection uses a Noise `XX` handshake with X25519, ChaCha20-Poly1305 and BLAKE2s, through a library and without a network extension or root.

After the handshake, every message is encrypted and authenticated with a per-direction nonce. Altered, replayed or reordered frames fail authentication. Each side's protocol version travels inside the handshake, so it is authenticated before either side acts on it.

Before trust negotiation, each side signs the completed Noise handshake hash and its connection role with its Secure Enclave P-256 key. Signatures use ECDSA with SHA-256 and DER encoding. Daisy checks the signature, then requires the device public key to match the key pinned with that peer’s Noise identity. The role prevents reflection and the handshake hash prevents replay on another session. Possession of the Noise private key alone does not authorize a connection.

## Developer diagnostics

Tracing is disabled by default. A same-account client can activate one local
collector through a mode-0600 Unix socket in Daisy's data directory. The CLI
creates a new mode-0600 NDJSON file and streams records into it.

The collector requests trace events from supported connected peers only after
Noise capability negotiation, device proof verification, and normal pairing-pin
and trust checks. Diagnostic traffic uses the existing encrypted sessions and
per-direction nonces. Each record is attributed to the authenticated sending key.
A trusted connected peer can request this system's application traces; the request
ends when its link ends. No unpaired endpoint or third-party service receives them.

Records and queues are bounded. One diagnostic frame may be unacknowledged per
link, and input and heartbeat queues take priority. Source and collector loss
counters expose dropped diagnostics. Capture callbacks enqueue copy-only facts
without formatting, waiting, or I/O. Added control diagnostics exclude keycodes,
input payloads, and clipboard contents; traces include public system keys, process
IDs, connection details, and errors. The normal stderr filter remains independent.

## Pairing

The handshake tells each side the other's long-term public key, but not whether it belongs to the intended peer. Pairing settles that once:

1. Both systems must be open to pairing. A sharing system is open while it has no peers yet; during **Add a System**, a 30-second window that closes after one success; or while **Always discoverable** is on, which stays open while sharing and requires the owner to authenticate with Touch ID or the login password to turn on. The CLI opens with `--pair`. A system that is not open refuses an unknown key without showing a code.
2. The connecting system types the code; the listening system shows a random six-digit code. A group member always opens the connection to a system with no peers, so the code appears on the newcomer. Between two systems with no peers, the one open to pairing longer opens; when they opened within two seconds of each other, their random advertisement nonces decide.
3. Both run SPAKE2 over the Ed25519 group, keyed by that code.
4. Each proves it derived the same key with HMAC-SHA256 over the Noise handshake hash, labelled by role. The role label stops reflection; the handshake hash binds the proof to this connection.
5. Only after both proofs verify does each side pin both the other’s Noise key and device public key.

An attacker in the middle has a different handshake with each side and does not know the code, so it gets one guess per attempt: a one-in-a-million chance. Comparing a short code by eye would not be safe because an attacker could grind keys until screens matched; the code is only ever PAKE input.

Each system runs one pairing exchange at a time and accepts at most five unknown-key attempts in a ten-minute window; an always-open gate gets a fresh allowance each window. After a pairing fails or is cancelled, a system waits 15 seconds before opening another, so a dismissed prompt does not return at once. **Add a System** is available whenever sharing is active, including connections by address, unless **Always discoverable** is on or an invitation is already open. Noise handshakes time out after 15 seconds; trust negotiation or code entry times out after two minutes. Choosing **Add a System** again, or restarting `listen --pair`, deliberately opens a new window.

## How long trust lasts

Trust is not forever by default. Each system keeps its own policy for every peer it trusts:

- **Idle**, the default: trust ends after a chosen number of hours or days without an authenticated session, 96 hours unless changed. A running session renews it.
- **A deadline**: N days, set only from the command line. Connecting does not extend it, and a session still running at the deadline ends.
- **Once**: trust ends with the session. Only after an accidental drop—silence or network error, not deliberate close—may the same key reconnect without a code, for at most 60 seconds.
- **Forever**: trust lasts until explicitly forgotten. It must be chosen and is always displayed as such.

Each system chooses the policy when pairing completes. Changing a policy restarts it from that moment, as if the peer had just paired. Each system enforces its own policy on its own clock. A session starts only while both peers trust each other, so neither can extend the other's trust by claiming that it still trusts.

Expired trust is removed from the peer file. The menu-bar app can forget a peer and aborts its active session task immediately; the peer-file watcher remains the backstop for changes made by another process and reacts within one second. The optional CLI provides `daisy forget` and `daisy rotate-key` for bulk revocation and identity rotation.

## Groups

A group is every system a member trusts, up to eight, each holding an encrypted session with every other. A new system joins by pairing with any one member; that member introduces it to the rest.

- **Device keys.** The same non-exportable Secure Enclave P-256 key signs session proofs, introductions and revocations. Pairing pins its public key beside the Noise identity. A pinned device key cannot change without forgetting the peer and pairing again.
- **Introductions.** An introduction names the introducer, the newcomer's Noise and signing keys, its name, the introducer's trust policy for it, and when the introducer began trusting it. It is signed by the introducer. A system accepts an introduction only from the member it names as introducer, with that member's pinned signing key, and not dated more than five minutes ahead of its own clock. It trusts the newcomer under whichever policy ends sooner: its own for the introducer, or the introducer's for the newcomer. Trust in an introduced system ends when trust in its introducer ends.
- **Revocations.** Forgetting one system signs a revocation naming it. Every member that trusts the signer removes that system and every system it introduced, keeps the revocation, and passes it on. An introduction dated before a known revocation of its newcomer is refused, so a member that has not yet heard cannot bring a revoked system back; pairing it again directly does. A system ignores a revocation of itself. Revocations are signed and saved before local peer removal; signing or persistence failure preserves the peer record for retry. `daisy forget --all` only leaves the group; it revokes nothing.
- **Catching up.** Whenever a session starts, each side sends the other an introduction of every system it trusts and every revocation it knows, so a member that was away catches up.

Introductions widen trust beyond the pairing a person performed: pairing A with B, and A with C, makes B and C trust each other. A member that is compromised, or with malware able to request its device signatures, can introduce a system of the attacker's choosing to every member, and can revoke members. Forgetting that member on any one system removes it, and everything it introduced, from the whole group.

## Discovery

Each system advertises `_daisy._tcp` with Bonjour under a random instance and host name. The TXT record holds a random 16-byte nonce and an 8-byte tag, the first 8 bytes of SHA-256 over a fixed label, the advertiser's public key and the nonce, plus whether pairing is open (`p`: closed, open with no peers, or open as a group member) and, when open, the Unix time it opened (`s`), used to choose who types the code. A peer that pinned the key recognises the tag; anyone else learns neither the key nor the advertiser's name, and each new advertisement uses a fresh nonce, so advertisements cannot be linked. An advertisement only suggests where to connect: every connection still runs the Noise handshake and the trust check, so a forged or replayed advertisement can at most send a connection somewhere it fails. The app advertises whenever it is sharing; `listen --no-discovery` turns advertising off from the command line.

## Reconnecting

Nearby paired peers find each other again with Bonjour after a drop, sleep or network change, and the elected opener connects; with a direct address, the system that connects keeps trying. Every attempt runs the full Noise handshake, device proof and trust check, so an expired or forgotten peer is refused exactly as on first contact. A reconnect is never allowed to pair: pairing is offered only until the first session starts. If the key that answers is not the key of the peer it was connected to, Daisy stops instead of retrying. Only network failures are retried (refused, unreachable, reset, silent or timed out); trust, key, protocol and setup failures stop.

## Failure containment

Input capture runs in the macOS event-tap callback, where blocking would lag the entire system. Its handoff queue is bounded and uses nonblocking sends. If the queue fills or callback state is contended, Daisy passes the current event through locally, reclaims local routing when possible, signals overload without taking another blocking lock and terminates the session.

Display activity updates are accepted only from the current control owner at the current generation, while the receiving system is unlocked and not handling local input. They refresh macOS’s display idle timeout without injecting input or unlocking a system. The owner sends updates only when it sees new physical input; session heartbeats do not postpone display sleep or locking.

Network reads and writes run in dedicated tasks behind bounded queues. Trust revocation, local overload and the three-second silence deadline remain selectable even when a peer stops reading. When the outgoing input channel closes, accepted messages have at most one second to flush; a blocked flush returns an error instead of reporting clean shutdown.

A system replaying its peer's input releases every held key and button whenever replay ends. A system sending input reclaims local control when its capture path overloads or the session ends.

## Clipboard

When clipboard sharing is on, a system sends its clipboard to the paired peer each time control crosses: plain text, rich text and images, up to 4 MB of text and 32 MB of image. It travels inside the encrypted session like input and is written straight to the receiving system's pasteboard. A paired peer can therefore read whatever was on the clipboard at the moment control crossed, which is within the trust pairing already grants. Either side can turn sharing off; a system with it off neither sends nor writes. The receiver discards any item larger than its limit or longer than it declared, and never writes a partial item.

Items marked concealed or transient by their source app stay on the system
where they were copied. This includes clipboard entries from password managers
that use the standard macOS markers.

## Keys at rest

Each system’s P-256 device private key stays in its Secure Enclave. The Data Protection Keychain persists it with `AfterFirstUnlockThisDeviceOnly` protection and private-key usage access control. It is non-exportable, does not synchronize, and cannot be restored onto another system. Signing does not require Touch ID or password prompts, so reconnects remain unattended. Malware running under the user’s account on the paired system can request signatures there.

The `device-identity` file holds a Keychain lookup reference, not private key material. The separate X25519 Noise key remains in `identity`. Both files are user-only (`0600`, under a `0700` directory). Copying the Noise key and the Keychain reference to another system does not supply the pinned device key. `daisy rotate-key` replaces both identities, retires the previous device key, and requires fresh pairing. A failed retirement restores the prior reference and reports an error. Unlock this system before creating or rotating a device key.

Device-bound peer trust is stored in `trust-v5/`. Peer records from before device-bound identity are not accepted or converted; those groups must pair again. Pinned public keys, introducers and trust policies live in `trust-v5/peers.toml`; signed revocations live in `trust-v5/revocations.toml`. Each update locks, rereads and rewrites the store, preventing a running process from restoring a peer another process forgot.

## Release integrity

The Homebrew cask pins the release DMG's SHA-256, and Homebrew verifies that
checksum before installation. It installs the same signed and notarized app;
device identity and trust checks do not depend on the installation method.

Releases are built only by the release workflow from a tagged commit, signed with Developer ID and notarized by Apple. The DMG and the app inside it are both notarized and stapled. Each DMG and zip carries SLSA Build Level 3 provenance and a GitHub artifact attestation. [releasing.md](releasing.md) explains how to verify both provenance and notarization.

When Daisy moves itself to Applications, Gatekeeper has already approved the running copy. Daisy copies that bundle with `ditto`, preserving its signature, and removes the quarantine attribute from the copy only, so the copy opens without App Translocation. Any copy it replaces goes to the Trash.

## Verified updates

The release installer reads public GitHub release assets and attestations over
HTTPS without an account or access token. Both the compatibility manifest and
ZIP archive must have a valid Sigstore signature and transparency proof from
this repository's release workflow at the selected version tag. Verification
uses bundled public Sigstore trust roots and the GitHub Actions OIDC issuer.
Signed SLSA provenance must bind the expected repository, workflow, tag, asset
name and SHA-256 digest. Unknown signing roots are rejected; the trust-root
snapshot is updated with the verifier dependency in subsequent releases.

The manifest must declare the selected version, the current session protocol,
and the exact ZIP SHA-256. Archive paths, entry types and expanded sizes are
checked before extraction into a private temporary directory. Before staging,
Daisy verifies the extracted app's notarization, bundle identifier, Developer
ID publisher and signed build version/protocol. The staged copy is checked
again before restart. Downloaded code is not executed during verification.
