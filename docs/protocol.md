# Protocol

Wire reference for implementers. For connection troubleshooting, see the [Troubleshooting](troubleshooting.md); for authentication decisions, see the [Security model](security-model.md).

How two peers talk. `src/protocol.rs` is the definition; this page explains it. If they disagree, the code is right.

## Transport

A TCP connection, to port 24850 by default. Every frame is a big-endian `u16` length followed by that many bytes of Noise ciphertext. Noise caps a message at 65,535 bytes including its 16 byte tag, so one encoded message is at most 65,519 bytes.

## Handshake

Every connection starts with a `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake, and both sides send long-term keys. The prologue is `daisy` and never changes. Each side includes its protocol generation in the first handshake message, followed by authenticated capabilities encoded as UTF-8 within the existing 64-byte limit. The capability list explicitly names supported generations, and peers select the greatest generation both support. A legacy peer can continue using the generation it sent while ignoring the capability suffix. Trace and file-transfer capabilities are negotiated independently. The capability tail is authenticated because it is bound into the device proof.

Both sides finish the authenticated handshake before comparing protocol support, so a peer can explain a mismatch. Different releases connect when they negotiate a shared protocol generation. Releases support the immediately preceding generation during rolling updates; older or unknown future generations are rejected unless explicitly supported. The handshake proves each side holds the private key for the public key it presented. It does not prove that key belongs to the peer you meant to reach; trust is settled next.

After the handshake the session splits into a sending half and a receiving half, so input can go out while messages arrive. Each direction keeps its own nonce, counting up from zero, so a replayed or reordered frame fails to authenticate.

After the Noise handshake, each side sends `DeviceProof { key, signature }` inside the encrypted transport. The key is a compressed SEC1 P-256 public key represented on the wire as a 32-byte x coordinate and an odd-y boolean. The DER-encoded ECDSA-SHA256 signature covers `daisy device session v1`, a role byte (0 initiator, 1 responder), then the completed Noise handshake hash. A missing or invalid proof closes the connection. Trust negotiation then requires a known peer’s device key to match its pairing pin, or authenticates both keys through new pairing.

## Messages

Messages are encoded with [postcard](https://github.com/jamesmunns/postcard), which identifies an enum variant by its position. **Variants are appended**: reordering or removing one makes peers on different versions misread each other, so it also raises the protocol version.

| Tag | Message | Sent by | Meaning |
|---|---|---|---|
| 0 | `Hello { name, trusts_you, will_pair }` | both, first | The sender's name, whether it already trusts the receiver's key, and whether it will pair now |
| 1 | `PairingKeyExchange { message }` | both | This side's SPAKE2 message, keyed by the one-time code |
| 2 | `PairingConfirmation { tag }` | both | Proof of the SPAKE2 key, bound to this session |
| 3 | `Ping { nonce }` | both | Heartbeat, every second |
| 4 | `Pong { nonce }` | both | Heartbeat reply |
| 5 | `Clipboard { part }` | both | A piece of the sender's clipboard, sent whenever control crosses, below |
| 6 | `Layout { side, chosen }` | both | The side of the sender the receiver was first placed on, and when, in Unix seconds; the later choice wins. Places a member that has no position in the arrangement yet |
| 7 | `ControlClaim { generation }` | any | The sender takes control at `generation` |
| 8 | `Enter { generation, to, at }` | system in control | The pointer crossed onto `to`, at `at` in its own coordinates |
| 9 | `Input { generation, event }` | system in control | One piece of input, below |
| 10 | `Leave { generation, to, at }` | system being driven | The pointer left for `to` at `at` in `to`'s coordinates: home to the system in control, or on to another member. A busy system sends it straight back |
| 11 | `Reclaim { generation }` | system in control | Control was taken back with the escape chord; release everything held |
| 12 | `ControlState { generation, owner }` | both, at start | Who the sender believes has control |
| 13 | `Displays { displays }` | both | The sender's displays in its own coordinates; sent at start and whenever one is added, removed or moved |
| 14 | `Arrangement { version, author, offsets }` | any | Where every member's displays sit in the group; the greatest `(version, author)` wins everywhere |
| 15 | `Locked { locked }` | both | Whether the sender's screen is locked; sent at start and on change |
| 16 | `SigningKey { key }` | reserved | Earlier signing-key message; not sent or accepted by the current protocol |
| 17 | `Introduce { introduction }` | any | A system the sender trusts, signed by the sender; see the security model |
| 18 | `Revoke { revocation }` | any | A system a member no longer trusts, signed by that member, passed on |
| 19 | `DeviceProof { key, signature }` | both, before Hello | Secure Enclave P-256 proof over the Noise handshake hash and connection role |
| 20 | `Activity { generation }` | system in control | New physical input; refreshes display activity on every unlocked member, at most once per second |
| 21 | `TraceControl { enabled }` | collector | Optional `trace-v1` request; enables trace events on this authenticated link until disabled or disconnected |
| 22 | `TraceRecord { record }` | requested peer | Optional bounded diagnostic record, at most 4 KiB as JSON; accepted only on a negotiated trace link |
| 23 | `TraceAck { sequence }` | collector | Optional acknowledgment of one trace record; only the outstanding sequence releases send credit |

`generation` is the sender's latest claim. Every member orders claims by `(generation, claimant key)` and keeps the greatest, so all agree on one owner whatever order claims arrive in. A system plays input only from the owner at the current generation, so input queued before a handoff never lands after it.

`Activity` is accepted only from the current owner at the current generation, while the receiving system is unlocked and not handling local input. It carries no input event and is not forwarded. Ordinary heartbeats do not count as user activity. When physical input stops, activity updates stop and each system’s normal display sleep and lock settings apply. Protocol generations increase monotonically when the wire contract changes. Releases keep the current generation compatible with the immediately preceding generation for rolling updates. Each release declares the generations it supports; numeric adjacency alone does not imply compatibility. Device identity and the trust store are unchanged by protocol negotiation.

Points are in a system's own coordinates: macOS global coordinates, origin at the top left of its main display, y growing downward. `Arrangement` places each system's displays by an offset into one shared space; displays of different systems never overlap there.

`ClipboardPart`, carried by `Clipboard`, follows the same append-only rule. A snapshot of the clipboard is its items, each a `Begin`, its `Chunk`s and an `End` sharing an `id`, followed by `Done`:

| Tag | Part | Fields |
|---|---|---|
| 0 | `Begin` | `id`, `kind` (`Text` for UTF-8 plain text, `Rtf` for rich text sent alongside its plain text, or `Png`), `len` in bytes |
| 1 | `Chunk` | `id`, up to 16,000 `bytes` |
| 2 | `End` | `id` |
| 3 | `Done` | none; the snapshot is complete |
| 4 | `Ack` | none; sent back for every `Chunk` received |

A snapshot holds a `Text` item, with an `Rtf` item when the copy has rich text, or a `Png` item. The receiver writes its pasteboard once, on `Done`, from the items that arrived whole. Text and rich text are limited to 4 MiB each and images to 32 MiB; a larger item is left out of the snapshot.

`InputEvent`, carried by `Input`, follows the same append-only rule:

| Tag | Event | Fields |
|---|---|---|
| 0 | `Motion` | pointer deltas `dx`, `dy` in points |
| 1 | `Button` | `button` (0 left, 1 right, 2 and up others), `down`, `clicks` as the driver counted them |
| 2 | `Scroll` | `dx`, `dy` in points |
| 3 | `Key` | macOS virtual key `code`, `down`, `repeat`, modifier `flags` |
| 4 | `Modifiers` | the modifier key `code` and the new `flags` |
| 5 | `Swipe` | one step of a trackpad swipe: `axis` (`Horizontal` or `Vertical`), `phase` (`Began`, `Changed`, `Ended` or `Cancelled`), `progress` in Spaces travelled and, when it ends, `velocity`, both signed as the trackpad reports them |
| 6 | `PhasedScroll` | trackpad scrolling: `dx`, `dy` in points and `phase`, below |

`Scroll` carries a scroll without a phase, such as a mouse wheel's. `PhasedScroll` carries a trackpad scroll and the momentum after a flick, so the receiver's apps scroll smoothly, coast and rubber-band. Its `phase` follows the same append-only rule: 0 `MayBegin`, 1 `Began`, 2 `Changed`, 3 `Ended`, 4 `Cancelled`, 5 `MomentumBegan`, 6 `Momentum`, 7 `MomentumEnded`. A scroll and its momentum stay on the system that had control when the fingers went down. The receiver replays a scroll only from its beginning, and ends one still under way when control leaves.

## Developer tracing

Only links where both handshake payloads advertised `trace-v1` may send tags
21–23. Fixed bytes in the protocol tests pin the trace record layout. A changed
optional layout requires a new capability version, independently of the required
sharing protocol. The normal trust and device-pinning checks complete before a link joins the
sharing group. A collector requests each connected peer directly; diagnostic
records are never relayed or attributed using an identity supplied in the record.
The collector labels each record with the authenticated sending key.

Trace queues are separate from input and clipboard queues. Enable requests and
acknowledgments use separate coalesced slots and consume no input queue capacity. Input and heartbeats
are selected first. At most one trace frame is unacknowledged per link, and stale
or duplicate acknowledgments release no additional credit. Saturation drops
traces with loss counters rather than terminating sharing. Disabling collection
discards already in-flight records while acknowledging them to drain the stream.
Dropping the requesting link releases its trace lease; tracing remains enabled
only if another request or local collector is active. Unsupported peers receive
none of the optional messages and remain connected normally.

## Copied files

Only links whose authenticated handshake payloads both advertise `files-v1` use tags 24–27. Existing input and clipboard wire encodings stay unchanged.

| Tag | Message | Connection | Purpose |
| --- | --- | --- | --- |
| 24 | `FilesOffer { offer }` | Input | Random 128-bit offer ID, bulk TCP port, item names, directory flags and content sizes |
| 25 | `FilesRelease { offer }` | Input | Destination releases its offer lease |
| 26 | `FilesRequest { offer, item }` | Bulk | Request an offered item by ID and zero-based index |
| 27 | `FilesPart { part }` | Bulk | Stream entries, contents, extended attributes and completion or failure |

Before allocating a bulk connection slot, the receiver of the connection checks that its source address belongs to an active group member, normalizing IPv4-mapped addresses. This admission check does not establish identity. A bulk connection performs a new Noise handshake and device proof with a five-second authentication deadline. Both pinned keys must match an active input-session member. During transfer, a successful store-backed trust check is reused for up to 250 ms, then rechecked at the next transfer step; connection membership and sharing enablement are checked without caching. The receiver connects to that member’s input-session address at the advertised bulk port. The sender accepts one item request per connection, only for an original recipient with a live lease. Unknown, expired, released, superseded and out-of-range requests are rejected; no path or enumeration request exists.

Offers go to all currently connected capable members. Later arrivals receive no earlier offer. Each destination has a 15-second idle lease, refreshed by requests and flowing bytes for that offer. Running transfers survive expiry or replacement, but replacement rejects new requests for the old offer. A clipboard change or completion of local caching releases the destination’s lease. Completed local file URLs are detached from the remote offer and stay on the clipboard until replaced, independently of lease expiry or peer disconnection.

`FilesPart` carries `Entry { path, kind, len, mode, compressed }`, `Data { bytes }`, `DataEnd`, `Attribute { name, len }`, `EntryEnd`, `Finished` or `Failed { reason }`. Paths are relative to the requested item; the first entry has an empty path. Kinds are file, directory or a relative symbolic link. An entry’s content ends at `DataEnd`; each following attribute has its own data stream and terminator, then `EntryEnd`. `Finished` is required before publication.

Offers are bounded to 64 items, 100,000 entries and 4 GiB of contents. Paths are at most 4,096 bytes and 128 components deep. Data chunks are at most 16,000 bytes. Extended attributes are bounded to 64 per entry and 32 MiB per item. Declared and decompressed sizes must agree; the receiver caps the zstd window at 8 MiB. Symbolic-link resolution is checked against the complete item manifest before publication. Streaming zstd applies before encryption, with an independent context per file: level 1 above 64 MiB, otherwise level 3. If the first 64 KiB sample does not shrink, contents are sent uncompressed. Input messages are never compressed.

The native clipboard adapter supplies local file URLs when Finder requests them. It may request them before Paste. A pending native request starts a worker and returns without a file URL. Finder’s Paste action becomes available when all items are cached and Daisy republishes their URLs, provided its offer still owns the clipboard. AppKit keeps servicing progress and cancellation. Transient listener failures retry with backoff, and rejected offers or an unavailable file service do not terminate the input link. Clipboard text fallback is retained for destinations without native file support. Files and directories are staged under hidden temporary names and published exclusively after contents and metadata complete; the top-level folder is renamed last. Completed cache entries survive clipboard replacement until the next boot so consumers can finish reading returned URLs.

## A group

Each system holds one session with every other member, up to eight systems. Sessions start independently: Bonjour shows which trusted members are nearby, and for each pair the lower key opens the connection.

## A session

1. The connecting peer and the listening peer complete the Noise handshake.
2. Both send `Hello`. If each already trusts the other's key, the session begins. Otherwise, if both are willing, they pair; if either is not, both drop the connection.
3. Both peers exchange `Layout` within five seconds. Their device keys have already been authenticated and pinned during trust negotiation. Each sends its signed introductions and revocations, then joins the session group.
4. Each sends `ControlState`, `Displays`, its `Arrangement` and `Locked`. A member with displays but no position is placed on the side `Layout` agreed, clear of every other member, as a new arrangement.
5. The system in control routes the pointer by geometry: leaving one of its own displays in a direction that reaches another member's display within 40 points sends `Enter` to that member, then `Input`. The member being driven does the same when the pointer leaves its displays, with `Leave` naming the next system; the system in control then sends `Enter` to it, or takes the pointer home. A locked member is never entered.
6. Whenever control crosses, the peer giving it up sends its clipboard as `Clipboard` parts, unless the receiving peer already has that clipboard change, because it was sent there before or received from there: the driver after `Enter`, the receiver after `Leave` or `Reclaim`, and a driver that receives a newer `ControlClaim`. A peer with clipboard sharing turned off sends nothing and does not write what arrives. A peer accepts one snapshot per crossing toward it and ignores clipboard parts at any other time. The receiver acknowledges every `Chunk` with `Ack`, even when it discards it, and the sender keeps at most four chunks unacknowledged, so no more than 64 KB of clipboard data sits ahead of input and heartbeats.
7. Both peers ping every second. Three seconds of silence ends the session and restores local capture and held-input state.

## Changing the protocol

Daisy release numbers and session protocol numbers are independent. Different
Daisy releases can share a group when their negotiated session protocol is
compatible. Discovery's TXT `v=1` identifies the beacon format, not the session
protocol or release number.

Keep the current protocol compatible across releases where possible. Preserve
the encoding, field order, enum tags, and meaning of every existing message
and nested wire type. This includes device proofs, signed introductions and
revocations, trust policy strings, control ownership, and input. The fixed bytes
in `tests/fixtures/protocol-6.txt` pin existing protocol-6 messages and input
enums; `tests/protocol_6.rs` checks encoding and decoding. Do not regenerate
those fixtures to accommodate a wire change.

An optional message variant is compatible only after both releases negotiate
support through an authenticated, backward-compatible exchange. Never infer
support from a release number or Bonjour advertisement, and never send an
optional variant unless both authenticated payloads advertised its capability.
A required message or behavior change requires a protocol change. Unknown
messages remain errors; silently ignoring control or security messages is
unsafe.

The current protocol generation explicitly supports its immediately preceding generation. The authenticated handshake advertises supported generations, and each link selects the greatest mutual generation before sharing begins. Numeric adjacency alone never authorizes compatibility. Unsupported older or unknown future generations are rejected. The update installer checks signed release metadata against this system and every currently connected peer. If an incompatible active peer disconnects, Daisy retries compatibility against the remaining group. Offline peers do not block an update; if one returns incompatible, Daisy rejects the connection and points to the canonical release page.

### Automatic installation contract

Packaging emits `Daisy-<version>-update.toml` metadata format `2`, binding the
exact Daisy release, primary session protocol, supported protocol list, and
SHA-256 of the final ZIP archive. Format 1 remains valid only for an exact
single-protocol match. The release workflow attests the metadata alongside the
archive and includes it in SLSA provenance. Missing, malformed, unsupported,
or unverified metadata does not authorize automatic installation.

Before replacing the app, the installer verifies the manifest provenance for
the repository's release workflow and selected tag, verifies the ZIP SHA-256,
and confirms the supported protocol list includes this system and every
currently connected peer. `update::Manifest::matches` checks the version, tag,
archive digest, and protocol metadata; it does not verify provenance or
authorize installation. The installer also verifies the app signature and
identity and retains the local device key, trust store, and settings.

Every automatic policy, including minor/patch-only, applies these checks.
Semver alone is not evidence of protocol compatibility. Only currently
connected peers gate an automatic protocol update; saved or disconnected peers
may need to update Daisy before reconnecting. The
verified `update` command enforces these checks before staging a release.
Both manifest and ZIP signatures are verified against the bundled public
Sigstore trust roots, the GitHub Actions issuer, and the exact repository,
release workflow, tag and artifact digest.

- Adding an optional message or event: append a variant and negotiate support through an authenticated, backward-compatible capability exchange before transmitting it. An older peer cannot decode an unknown tag and ends the session.
- Adding a required message or event: append a variant and raise `PROTOCOL` in `src/session.rs` before transmitting it.
- Anything that changes the meaning of an existing message, or removes or reorders one: raise `PROTOCOL` in `src/session.rs`, so mismatched peers stop at the handshake and say which to update, rather than misbehave.

A physical event on any member claims ownership with a newer generation, sent to every member. The 150 ms settle window limits repeated claims when members are used together; equal generations favor the greater key. Remote injection is suppressed during local physical activity. When the member in control leaves the group, the others take control back locally. A change of driver preserves the arrangement.
