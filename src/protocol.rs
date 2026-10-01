//! Messages exchanged inside an encrypted session.
//!
//! Encoded with postcard, which identifies enum variants by position:
//! append new variants at the end and never reorder or remove existing ones,
//! or peers on different versions will misread each other.

use serde::{Deserialize, Serialize};

use crate::identity::PublicKey;
use crate::input::{InputEvent, Point, Rect, Side};

pub const CONFIRMATION_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Message {
    /// First message each side sends. `trusts_you` says whether the sender
    /// has already paired with the receiver's key, `will_pair` whether it is
    /// willing to pair now if needed.
    Hello {
        name: String,
        trusts_you: bool,
        will_pair: bool,
    },
    /// This side's SPAKE2 message, keyed by the one-time pairing code.
    PairingKeyExchange {
        message: Vec<u8>,
    },
    /// Proof that this side derived the same pairing key, bound to the session.
    PairingConfirmation {
        tag: [u8; CONFIRMATION_LEN],
    },
    Ping {
        nonce: u64,
    },
    Pong {
        nonce: u64,
    },
    /// A piece of the sender's clipboard, sent whenever control crosses.
    Clipboard {
        part: ClipboardPart,
    },
    /// Where the sender places the receiver, and when that was chosen.
    Layout {
        side: Side,
        chosen: u64,
    },
    /// The sender takes control, as its `generation`th claim.
    ControlClaim {
        generation: u64,
    },
    /// The pointer crossed onto the receiver `to`, at `at` in its own
    /// coordinates.
    Enter {
        generation: u64,
        to: PublicKey,
        at: Point,
    },
    Input {
        generation: u64,
        event: InputEvent,
    },
    /// The pointer left the receiver for the system `to`, which may be the
    /// one in control, at `at` in that system's own coordinates.
    Leave {
        generation: u64,
        to: PublicKey,
        at: Point,
    },
    /// The system in control took the pointer back; release everything held.
    Reclaim {
        generation: u64,
    },
    /// Who the sender believes has control, sent when a session starts, so a
    /// system joining a group learns the current owner and generation.
    ControlState {
        generation: u64,
        owner: PublicKey,
    },
    /// The sender's displays in its own coordinates, sent when a session
    /// starts and whenever they change.
    Displays {
        displays: Vec<Rect>,
    },
    /// Where every system's displays sit in the group, as a whole. The
    /// greatest `(version, author)` wins everywhere.
    Arrangement {
        version: u64,
        author: PublicKey,
        offsets: Vec<(PublicKey, (f64, f64))>,
    },
    /// Whether the sender's screen is locked, sent when a session starts and
    /// whenever it changes. Input sent to a locked system does nothing.
    Locked {
        locked: bool,
    },
    /// The key the sender signs introductions and revocations with, sent
    /// once when a session starts.
    SigningKey {
        key: [u8; 32],
    },
    /// The sender trusts a system, and introduces it, signed.
    Introduce {
        introduction: crate::introduce::Signed<crate::introduce::Introduction>,
    },
    /// A member no longer trusts a system, signed by that member.
    Revoke {
        revocation: crate::introduce::Signed<crate::introduce::Revocation>,
    },
}

/// A clipboard snapshot is its items, each a `Begin`, its `Chunk`s and an
/// `End`, followed by `Done`. Same rule as `Message`: append variants, never
/// reorder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClipboardPart {
    /// Starts one item; `len` is its full byte length and `id` ties its
    /// chunks together.
    Begin {
        id: u32,
        kind: ClipboardKind,
        len: u32,
    },
    Chunk {
        id: u32,
        bytes: Vec<u8>,
    },
    End {
        id: u32,
    },
    /// The snapshot is complete; the receiver writes it now.
    Done,
    /// Sent back for every `Chunk` received, so the sender keeps only a few
    /// chunks in flight and input and heartbeats never queue behind them.
    Ack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipboardKind {
    /// UTF-8 plain text.
    Text,
    /// Rich text, sent alongside its plain text.
    Rtf,
    Png,
}

impl Message {
    pub fn encode(&self) -> Result<Vec<u8>, postcard::Error> {
        postcard::to_stdvec(self)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, postcard::Error> {
        postcard::from_bytes(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let messages = [
            Message::Hello {
                name: "Studio".into(),
                trusts_you: true,
                will_pair: false,
            },
            Message::PairingKeyExchange { message: vec![1, 2, 3] },
            Message::PairingConfirmation {
                tag: [7; CONFIRMATION_LEN],
            },
            Message::Ping { nonce: u64::MAX },
            Message::Pong { nonce: 0 },
            Message::Layout {
                side: Side::Left,
                chosen: u64::MAX,
            },
            Message::ControlClaim { generation: 3 },
            Message::Enter {
                generation: 3,
                to: crate::identity::PublicKey::from_bytes(&[9; 32]).unwrap(),
                at: (12.5, -3.0),
            },
            Message::Input {
                generation: 3,
                event: InputEvent::Key {
                    code: 0,
                    down: true,
                    repeat: false,
                    flags: 0x0010_0000,
                },
            },
            Message::Leave {
                generation: 3,
                to: crate::identity::PublicKey::from_bytes(&[8; 32]).unwrap(),
                at: (-1440.0, 900.0),
            },
            Message::Displays {
                displays: vec![Rect {
                    x: -800.0,
                    y: -1440.0,
                    width: 2560.0,
                    height: 1440.0,
                }],
            },
            Message::Arrangement {
                version: 7,
                author: crate::identity::PublicKey::from_bytes(&[1; 32]).unwrap(),
                offsets: vec![(crate::identity::PublicKey::from_bytes(&[2; 32]).unwrap(), (1512.0, 0.0))],
            },
            Message::Reclaim { generation: 3 },
            Message::Clipboard {
                part: ClipboardPart::Begin {
                    id: 1,
                    kind: ClipboardKind::Rtf,
                    len: u32::MAX,
                },
            },
            Message::Clipboard {
                part: ClipboardPart::Chunk {
                    id: 1,
                    bytes: vec![0, 255, 7],
                },
            },
            Message::Clipboard {
                part: ClipboardPart::End { id: 1 },
            },
            Message::Clipboard {
                part: ClipboardPart::Done,
            },
            Message::Clipboard {
                part: ClipboardPart::Ack,
            },
        ];
        for message in messages {
            assert_eq!(Message::decode(&message.encode().unwrap()).unwrap(), message);
        }
    }

    #[test]
    fn variant_tags_are_stable() {
        // the first byte is the variant index; changing these breaks older peers
        assert_eq!(
            Message::Hello {
                name: String::new(),
                trusts_you: false,
                will_pair: false,
            }
            .encode()
            .unwrap()[0],
            0
        );
        assert_eq!(Message::PairingKeyExchange { message: vec![] }.encode().unwrap()[0], 1);
        assert_eq!(
            Message::PairingConfirmation {
                tag: [0; CONFIRMATION_LEN],
            }
            .encode()
            .unwrap()[0],
            2
        );
        assert_eq!(Message::Ping { nonce: 0 }.encode().unwrap()[0], 3);
        assert_eq!(Message::Pong { nonce: 0 }.encode().unwrap()[0], 4);
        let clipboard = Message::Clipboard {
            part: ClipboardPart::End { id: 0 },
        }
        .encode()
        .unwrap();
        assert_eq!(clipboard[0], 5);
        // the second byte is the part's own variant index
        assert_eq!(clipboard[1], 2);
        // tag, part tag, id (0 fits one byte), then the kind's own index
        assert_eq!(
            Message::Clipboard {
                part: ClipboardPart::Begin {
                    id: 0,
                    kind: ClipboardKind::Png,
                    len: 0,
                },
            }
            .encode()
            .unwrap()[1..=3],
            [0, 0, 2]
        );
        assert_eq!(
            Message::Clipboard {
                part: ClipboardPart::Chunk { id: 0, bytes: vec![] },
            }
            .encode()
            .unwrap()[1],
            1
        );
        assert_eq!(
            Message::Clipboard {
                part: ClipboardPart::Done,
            }
            .encode()
            .unwrap()[1],
            3
        );
        assert_eq!(
            Message::Clipboard {
                part: ClipboardPart::Ack,
            }
            .encode()
            .unwrap()[1],
            4
        );
        let layout = Message::Layout {
            side: Side::Left,
            chosen: 0,
        };
        assert_eq!(layout.encode().unwrap()[0], 6);
        assert_eq!(Message::ControlClaim { generation: 0 }.encode().unwrap()[0], 7);
        let key = crate::identity::PublicKey::from_bytes(&[0; 32]).unwrap();
        assert_eq!(
            Message::Enter {
                generation: 0,
                to: key,
                at: (0.0, 0.0)
            }
            .encode()
            .unwrap()[0],
            8
        );
        let input = Message::Input {
            generation: 0,
            event: InputEvent::Motion { dx: 0.0, dy: 0.0 },
        };
        assert_eq!(input.encode().unwrap()[0], 9);
        assert_eq!(
            Message::Leave {
                generation: 0,
                to: key,
                at: (0.0, 0.0)
            }
            .encode()
            .unwrap()[0],
            10
        );
        assert_eq!(Message::Reclaim { generation: 0 }.encode().unwrap()[0], 11);
        let state = Message::ControlState {
            generation: 0,
            owner: crate::identity::PublicKey::from_bytes(&[0; 32]).unwrap(),
        };
        assert_eq!(state.encode().unwrap()[0], 12);
        assert_eq!(Message::Displays { displays: vec![] }.encode().unwrap()[0], 13);
        let arrangement = Message::Arrangement {
            version: 0,
            author: key,
            offsets: vec![],
        };
        assert_eq!(arrangement.encode().unwrap()[0], 14);
        assert_eq!(Message::Locked { locked: true }.encode().unwrap()[0], 15);
        assert_eq!(Message::SigningKey { key: [0; 32] }.encode().unwrap()[0], 16);
        let introduction = Message::Introduce {
            introduction: crate::introduce::Signed {
                body: crate::introduce::Introduction {
                    introducer: key,
                    newcomer: key,
                    newcomer_signing: [0; 32],
                    name: String::new(),
                    policy: crate::trust::Policy::Forever,
                    trusted_since: 0,
                },
                signature: vec![],
            },
        };
        assert_eq!(introduction.encode().unwrap()[0], 17);
        let revocation = Message::Revoke {
            revocation: crate::introduce::Signed {
                body: crate::introduce::Revocation {
                    by: key,
                    revoked: key,
                    at: 0,
                },
                signature: vec![],
            },
        };
        assert_eq!(revocation.encode().unwrap()[0], 18);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Message::decode(&[0xff, 0xff]).is_err());
        assert!(Message::decode(&[]).is_err());
    }
}
