//! Fixed protocol-6 bytes, independent of the current encoder and decoder.
//! Keep these fixtures when extending the protocol; never regenerate them to
//! make a compatibility failure pass.

use daisy::device;
use daisy::identity::PublicKey;
use daisy::input::{InputEvent, Rect, ScrollPhase, Side};
use daisy::introduce::{Introduction, Revocation, Signed};
use daisy::protocol::{ClipboardKind, ClipboardPart, Message};
use daisy::swipe::{SwipeAxis, SwipePhase, SwipeStep};
use daisy::trust::Policy;

// Exhaustive patterns force additions to any wire enum to revisit this contract.
// A new sender must not silently keep protocol 6 while old peers reject its tag.
fn require_baseline_variant(message: &Message) {
    match message {
        Message::Hello { .. }
        | Message::PairingKeyExchange { .. }
        | Message::PairingConfirmation { .. }
        | Message::Ping { .. }
        | Message::Pong { .. }
        | Message::ControlClaim { .. }
        | Message::Enter { .. }
        | Message::Leave { .. }
        | Message::Reclaim { .. }
        | Message::ControlState { .. }
        | Message::Displays { .. }
        | Message::Arrangement { .. }
        | Message::Locked { .. }
        | Message::SigningKey { .. }
        | Message::Revoke { .. }
        | Message::DeviceProof { .. }
        | Message::Activity { .. } => {}
        Message::Clipboard { part } => match part {
            ClipboardPart::Begin { kind, .. } => match kind {
                ClipboardKind::Text | ClipboardKind::Rtf | ClipboardKind::Png => {}
            },
            ClipboardPart::Chunk { .. } | ClipboardPart::End { .. } | ClipboardPart::Done | ClipboardPart::Ack => {}
        },
        Message::Layout { side, .. } => match side {
            Side::Left | Side::Right | Side::Above | Side::Below => {}
        },
        Message::Introduce { introduction } => match introduction.body.policy {
            Policy::Idle(_) | Policy::Days(_) | Policy::Once | Policy::Forever => {}
        },
        Message::Input { event, .. } => match event {
            InputEvent::Motion { .. }
            | InputEvent::Button { .. }
            | InputEvent::Scroll { .. }
            | InputEvent::Key { .. }
            | InputEvent::Modifiers { .. } => {}
            InputEvent::Swipe { step } => {
                match step.axis {
                    SwipeAxis::Horizontal | SwipeAxis::Vertical => {}
                }
                match step.phase {
                    SwipePhase::Began | SwipePhase::Changed | SwipePhase::Ended | SwipePhase::Cancelled => {}
                }
            }
            InputEvent::PhasedScroll { phase, .. } => match phase {
                ScrollPhase::MayBegin
                | ScrollPhase::Began
                | ScrollPhase::Changed
                | ScrollPhase::Ended
                | ScrollPhase::Cancelled
                | ScrollPhase::MomentumBegan
                | ScrollPhase::Momentum
                | ScrollPhase::MomentumEnded => {}
            },
        },
    }
}

fn messages() -> Vec<(String, Message)> {
    let key = PublicKey::from_bytes(&[0x31; 32]).unwrap();
    let other = PublicKey::from_bytes(&[0x72; 32]).unwrap();
    // SEC1 encoding of the P-256 generator; no signing key is needed.
    let device = device::PublicKey::from_bytes(&[
        3, 107, 23, 209, 242, 225, 44, 66, 71, 248, 188, 230, 229, 99, 164, 64, 242, 119, 3, 125, 129, 45, 235, 51,
        160, 244, 161, 57, 69, 216, 152, 194, 150,
    ])
    .unwrap();
    let mut cases = Vec::new();
    let mut add = |name: &str, message| cases.push((name.to_owned(), message));
    add(
        "hello",
        Message::Hello {
            name: "Studio 🌼".into(),
            trusts_you: true,
            will_pair: false,
        },
    );
    add(
        "pairing-key",
        Message::PairingKeyExchange {
            message: vec![0, 128, 255],
        },
    );
    add("pairing-confirmation", Message::PairingConfirmation { tag: [0xa5; 32] });
    add("ping", Message::Ping { nonce: u64::MAX });
    add("pong", Message::Pong { nonce: 128 });
    for kind in [ClipboardKind::Text, ClipboardKind::Rtf, ClipboardKind::Png] {
        add(
            &format!("clipboard-{kind:?}"),
            Message::Clipboard {
                part: ClipboardPart::Begin {
                    id: 129,
                    kind,
                    len: u32::MAX,
                },
            },
        );
    }
    for (name, part) in [
        (
            "chunk",
            ClipboardPart::Chunk {
                id: 129,
                bytes: vec![0, 128, 255],
            },
        ),
        ("end", ClipboardPart::End { id: 129 }),
        ("done", ClipboardPart::Done),
        ("ack", ClipboardPart::Ack),
    ] {
        add(&format!("clipboard-{name}"), Message::Clipboard { part });
    }
    for side in [Side::Left, Side::Right, Side::Above, Side::Below] {
        add(&format!("layout-{side:?}"), Message::Layout { side, chosen: 129 });
    }
    add("claim", Message::ControlClaim { generation: u64::MAX });
    add(
        "enter",
        Message::Enter {
            generation: 129,
            to: key,
            at: (-12.5, 900.0),
        },
    );
    add(
        "leave",
        Message::Leave {
            generation: 130,
            to: other,
            at: (1440.0, -3.25),
        },
    );
    add("reclaim", Message::Reclaim { generation: 131 });
    add(
        "control-state",
        Message::ControlState {
            generation: 132,
            owner: key,
        },
    );
    add(
        "displays",
        Message::Displays {
            displays: vec![
                Rect {
                    x: -800.0,
                    y: -1440.0,
                    width: 2560.0,
                    height: 1440.0,
                },
                Rect {
                    x: 0.0,
                    y: 0.0,
                    width: 1512.0,
                    height: 982.0,
                },
            ],
        },
    );
    add(
        "arrangement",
        Message::Arrangement {
            version: 133,
            author: key,
            offsets: vec![(key, (-1512.0, 4.5)), (other, (0.0, -900.0))],
        },
    );
    add("locked", Message::Locked { locked: true });
    add("unlocked", Message::Locked { locked: false });
    add("reserved-signing-key", Message::SigningKey { key: [0xa6; 32] });
    for policy in [Policy::Idle(96), Policy::Days(30), Policy::Once, Policy::Forever] {
        add(
            &format!("introduce-{policy:?}"),
            Message::Introduce {
                introduction: Signed {
                    body: Introduction {
                        introducer: key,
                        newcomer: other,
                        newcomer_signing: device,
                        name: "Desk 🌼".into(),
                        policy,
                        trusted_since: 134,
                    },
                    signature: vec![0x30, 0x81, 0xff],
                },
            },
        );
    }
    add(
        "revoke",
        Message::Revoke {
            revocation: Signed {
                body: Revocation {
                    by: key,
                    revoked: other,
                    at: 135,
                },
                signature: vec![0x30, 0x82, 0xfe],
            },
        },
    );
    add(
        "device-proof",
        Message::DeviceProof {
            key: device,
            signature: vec![0x30, 0x83, 0xfd],
        },
    );
    add("activity", Message::Activity { generation: 136 });
    for (name, event) in [
        ("motion", InputEvent::Motion { dx: -12.5, dy: 3.25 }),
        (
            "button",
            InputEvent::Button {
                button: 2,
                down: true,
                clicks: 3,
            },
        ),
        ("scroll", InputEvent::Scroll { dx: 4.5, dy: -6.75 }),
        (
            "key",
            InputEvent::Key {
                code: 129,
                down: false,
                repeat: true,
                flags: 0x100000,
            },
        ),
        (
            "modifiers",
            InputEvent::Modifiers {
                code: 130,
                flags: 0x200000,
            },
        ),
    ] {
        add(&format!("input-{name}"), Message::Input { generation: 137, event });
    }
    for axis in [SwipeAxis::Horizontal, SwipeAxis::Vertical] {
        for phase in [
            SwipePhase::Began,
            SwipePhase::Changed,
            SwipePhase::Ended,
            SwipePhase::Cancelled,
        ] {
            add(
                &format!("swipe-{axis:?}-{phase:?}"),
                Message::Input {
                    generation: 138,
                    event: InputEvent::Swipe {
                        step: SwipeStep {
                            axis,
                            phase,
                            progress: -0.75,
                            velocity: 1.25,
                        },
                    },
                },
            );
        }
    }
    for phase in [
        ScrollPhase::MayBegin,
        ScrollPhase::Began,
        ScrollPhase::Changed,
        ScrollPhase::Ended,
        ScrollPhase::Cancelled,
        ScrollPhase::MomentumBegan,
        ScrollPhase::Momentum,
        ScrollPhase::MomentumEnded,
    ] {
        add(
            &format!("phased-scroll-{phase:?}"),
            Message::Input {
                generation: 139,
                event: InputEvent::PhasedScroll {
                    dx: -7.5,
                    dy: 8.25,
                    phase,
                },
            },
        );
    }
    cases
}

#[test]
fn protocol_6_wire_contract() {
    let messages = messages();
    let fixtures: Vec<_> = include_str!("fixtures/protocol-6.txt").lines().collect();
    assert_eq!(messages.len(), fixtures.len());
    for ((name, message), fixture) in messages.into_iter().zip(fixtures) {
        require_baseline_variant(&message);
        let (saved_name, hex) = fixture.split_once(' ').unwrap();
        assert_eq!(name, saved_name);
        let bytes: Vec<u8> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        assert_eq!(message.encode().unwrap(), bytes, "encoding changed: {name}");
        assert_eq!(Message::decode(&bytes).unwrap(), message, "decoding changed: {name}");
    }
}
