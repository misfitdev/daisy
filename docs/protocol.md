# Protocol

How two peers talk. `src/protocol.rs` is the definition; this page explains it. If they disagree, the code is right.

## Transport

A TCP connection, to port 24850 by default. Every frame is a big-endian `u16` length followed by that many bytes of Noise ciphertext. Noise caps a message at 65,535 bytes including its 16 byte tag, so one encoded message is at most 65,519 bytes.

## Handshake

Every connection starts a `Noise_XX_25519_ChaChaPoly_BLAKE2s` handshake in which both sides send their long-term keys. The prologue is `daisy/2`, so a peer speaking an incompatible protocol version fails the handshake instead of misreading messages. Daisy then reports that the peer runs a different version of Daisy and that both systems need updating.

The handshake proves each side holds the private key for the public key it presented. It does not prove that key belongs to the peer you meant to reach; trust is settled next.

After the handshake the session splits into a sending half and a receiving half, so input can go out while messages arrive. Each direction keeps its own nonce, counting up from zero, so a replayed or reordered frame fails to authenticate.

## Messages

Messages are encoded with [postcard](https://github.com/jamesmunns/postcard), which identifies an enum variant by its position. **Variants are only ever appended**: reordering or removing one would make peers on different versions misread each other.

| Tag | Message | Sent by | Meaning |
|---|---|---|---|
| 0 | `Hello { name, trusts_you, will_pair }` | both, first | The sender's name, whether it already trusts the receiver's key, and whether it will pair now |
| 1 | `PairingKeyExchange { message }` | both | This side's SPAKE2 message, keyed by the one-time code |
| 2 | `PairingConfirmation { tag }` | both | Proof of the SPAKE2 key, bound to this session |
| 3 | `Ping { nonce }` | both | Heartbeat, every second |
| 4 | `Pong { nonce }` | both | Heartbeat reply |
| 5 | `Drive { side }` | driver | It has the keyboard and mouse; the receiver sits on `side` of it |
| 6 | `Enter { along }` | driver | The pointer crossed onto the receiver's screen |
| 7 | `Input { event }` | driver | One piece of input, below |
| 8 | `Leave { along }` | receiver | The pointer went back out through the shared edge |
| 9 | `Reclaim` | driver | Control was taken back with the escape chord; release everything |
| 10 | `Clipboard { part }` | both | A piece of the sender's clipboard, sent whenever control crosses, below |
| 11 | `Layout { side, chosen }` | both | Where the sender places the receiver, and when that side was chosen in Unix seconds; the later choice wins |
| 12 | `ControlClaim { generation }` | either | Claim the input driver role; equal generations favor the initiator |
| 13 | `SharedEnter { generation, along }` | current driver | Enter the other screen |
| 14 | `SharedInput { generation, event }` | current driver | Input belonging to this generation |
| 15 | `SharedLeave { generation, along }` | receiver | Return the driver's pointer |
| 16 | `SharedReclaim { generation }` | current driver | Release remote held input |

Tags 5–9 are reserved for the original one-way session and are no longer sent by current versions. Current sessions use tags 11–16.

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

`along` is a position on the shared edge, from 0 at its top or left end to 65,535 at its bottom or right end, so screens of different sizes line up proportionally.

## A session

1. The connecting peer and the listening peer complete the Noise handshake.
2. Both send `Hello`. If each already trusts the other's key, the session begins. Otherwise, if both are willing, they pair; if either is not, both drop the connection.
3. Both peers exchange `Layout` within five seconds. The side chosen most recently wins, with the initiator's winning a tie; both store the agreed relation and when it was chosen.
4. Pushing the pointer past the shared edge sends `SharedEnter`, then `SharedInput`. Pushing it back across the shared edge sends `SharedLeave`.
5. Whenever control crosses, the peer giving it up sends its clipboard as `Clipboard` parts: the driver after `SharedEnter`, the receiver after `SharedLeave` or `SharedReclaim`, and a driver that receives a newer `ControlClaim`. A peer with clipboard sharing turned off sends nothing and does not write what arrives. A peer accepts one snapshot per crossing toward it and ignores clipboard parts at any other time. The receiver acknowledges every `Chunk` with `Ack`, even when it discards it, and the sender keeps at most four chunks unacknowledged, so no more than 64 KB of clipboard data sits ahead of input and heartbeats.
6. Both peers ping every second. Three seconds of silence ends the session and restores local capture and held-input state.

## Changing the protocol

- Adding a message or event: append a variant. An older peer cannot decode an unknown tag and ends the session, so only send a new message to a peer known to understand it.
- Anything that changes the meaning of an existing message: bump the prologue, so mismatched peers fail the handshake rather than misbehave.

A physical event on either peer claims ownership with a newer generation. The 150 ms settle window limits repeated claims when both peers are used together; equal generations favor the connection initiator. Remote injection is suppressed during local physical activity. Shared messages with an older generation are ignored. A change of driver preserves the screen relation.
