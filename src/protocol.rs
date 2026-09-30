//! Messages exchanged inside an encrypted session.
//!
//! Encoded with postcard, which identifies enum variants by position:
//! append new variants at the end and never reorder or remove existing ones,
//! or peers on different versions will misread each other.

use serde::{Deserialize, Serialize};

use crate::input::{Along, InputEvent, Side};

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
    /// The pointer crossed onto the receiver's screen at `along`.
    Enter {
        generation: u64,
        along: Along,
    },
    Input {
        generation: u64,
        event: InputEvent,
    },
    /// The pointer went back to the system in control at `along`.
    Leave {
        generation: u64,
        along: Along,
    },
    /// The system in control took the pointer back; release everything held.
    Reclaim {
        generation: u64,
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
                along: 12,
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
                along: u16::MAX,
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
        assert_eq!(
            Message::Enter {
                generation: 0,
                along: 0
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
                along: 0
            }
            .encode()
            .unwrap()[0],
            10
        );
        assert_eq!(Message::Reclaim { generation: 0 }.encode().unwrap()[0], 11);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Message::decode(&[0xff, 0xff]).is_err());
        assert!(Message::decode(&[]).is_err());
    }
}
