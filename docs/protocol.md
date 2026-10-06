# Protocol

Wire reference for implementers. For connection troubleshooting, see the [Troubleshooting](troubleshooting.md); for authentication decisions, see the [Security model](security-model.md).

How two peers talk. `src/protocol.rs` is the definition; this page explains it. If they disagree, the code is right.

## Transport

A TCP connection, to port 24850 by default. Every frame is a big-endian `u16` length followed by that many bytes of Noise ciphertext. Noise caps a message at 65,535 bytes including its 16 byte tag, so one encoded message is at most 65,519 bytes.

## Handshake

Every connection starts a `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake in which both sides send their long-term keys. The prologue is `daisy` and never changes.

Each side carries its version in the payload of its first handshake message: the protocol version as two big-endian bytes, then the Daisy release in UTF-8, at most 64 bytes. Every version must read this layout. The current protocol version is 6. Both sides finish the handshake before comparing, so the versions are authenticated and each side can explain a mismatch. Different releases on the same protocol version connect. Different protocol versions do not: each side names both versions and says which system to update, the one on the lower protocol version.

The handshake proves each side holds the private key for the public key it presented. It does not prove that key belongs to the peer you meant to reach; trust is settled next.

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

`generation` is the sender's latest claim. Every member orders claims by `(generation, claimant key)` and keeps the greatest, so all agree on one owner whatever order claims arrive in. A system plays input only from the owner at the current generation, so input queued before a handoff never lands after it.

`Activity` is accepted only from the current owner at the current generation, while the receiving system is unlocked and not handling local input. It carries no input event and is not forwarded. Ordinary heartbeats do not count as user activity. When physical input stops, activity updates stop and each system’s normal display sleep and lock settings apply. Protocol 6 requires every connected member to use protocol 6; the existing device identity and trust store are unchanged.

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

## A group

Each system holds one session with every other member, up to eight systems. Sessions start independently: Bonjour shows which trusted members are nearby, and for each pair the lower key opens the connection.

## A session

1. The connecting peer and the listening peer complete the Noise handshake.
2. Both send `Hello`. If each already trusts the other's key, the session begins. Otherwise, if both are willing, they pair; if either is not, both drop the connection.
3. Both peers exchange `Layout` within five seconds. Their device keys have already been authenticated and pinned during trust negotiation. Each sends its signed introductions and revocations, then joins the session group.
4. Each sends `ControlState`, `Displays`, its `Arrangement` and `Locked`. A member with displays but no position is placed on the side `Layout` agreed, clear of every other member, as a new arrangement.
5. The system in control routes the pointer by geometry: leaving one of its own displays in a direction that reaches another member's display within 40 points sends `Enter` to that member, then `Input`. The member being driven does the same when the pointer leaves its displays, with `Leave` naming the next system; the system in control then sends `Enter` to it, or takes the pointer home. A locked member is never entered.
6. Whenever control crosses, the peer giving it up sends its clipboard as `Clipboard` parts: the driver after `Enter`, the receiver after `Leave` or `Reclaim`, and a driver that receives a newer `ControlClaim`. A peer with clipboard sharing turned off sends nothing and does not write what arrives. A peer accepts one snapshot per crossing toward it and ignores clipboard parts at any other time. The receiver acknowledges every `Chunk` with `Ack`, even when it discards it, and the sender keeps at most four chunks unacknowledged, so no more than 64 KB of clipboard data sits ahead of input and heartbeats.
7. Both peers ping every second. Three seconds of silence ends the session and restores local capture and held-input state.

## Changing the protocol

- Adding a message or event: append a variant. An older peer cannot decode an unknown tag and ends the session, so only send a new message to a peer known to understand it.
- Anything that changes the meaning of an existing message, or removes or reorders one: raise `PROTOCOL` in `src/session.rs`, so mismatched peers stop at the handshake and say which to update, rather than misbehave.

A physical event on any member claims ownership with a newer generation, sent to every member. The 150 ms settle window limits repeated claims when members are used together; equal generations favor the greater key. Remote injection is suppressed during local physical activity. When the member in control leaves the group, the others take control back locally. A change of driver preserves the arrangement.
