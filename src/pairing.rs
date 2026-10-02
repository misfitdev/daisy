//! Deciding whether to trust the key at the other end of a session.
//!
//! Systems that have paired recognize each other's pinned keys and go straight
//! to work. Otherwise, and only when both sides opt in, they pair: the system
//! that accepted the connection shows a one-time code, and it is typed on
//! the system that opened it. Both run SPAKE2 keyed by the code, then prove
//! they derived the same key over this session's handshake hash.
//!
//! An attacker in the middle has a different handshake with each side and
//! does not know the code, so it gets one guess per attempt, a one in a
//! million chance, and a wrong guess aborts pairing. A short code compared
//! by eye would not be safe: the attacker could try keys until the two
//! displayed codes matched.

use std::fmt;
use std::future::Future;

use anyhow::{Context, Result, bail};
use hmac::{Hmac, Mac};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::peers::PeerStore;
use crate::protocol::{CONFIRMATION_LEN, Message};
use crate::session::{Channel, Role};
use crate::trust::{self, Policy};

const CODE_SPACE: u32 = 1_000_000;
const MAX_PEER_NAME_CHARS: usize = 80;
const SPAKE_IDENTITY: &[u8] = b"daisy pairing v1";
const CONFIRMATION_LABEL: &[u8] = b"daisy pairing confirmation v1";

/// Six digit one-time code, shown on one system and typed on the other.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PairingCode(u32);

impl PairingCode {
    pub fn random() -> Self {
        // reject the top of the range so every code is equally likely
        let limit = u32::MAX - u32::MAX % CODE_SPACE;
        loop {
            let value = OsRng.next_u32();
            if value < limit {
                return Self(value % CODE_SPACE);
            }
        }
    }

    /// Accepts the digits with or without separating spaces or dashes.
    pub fn parse(text: &str) -> Option<Self> {
        let digits: String = text.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
        if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
        digits.parse().ok().map(Self)
    }

    fn digits(&self) -> String {
        format!("{:06}", self.0)
    }
}

impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.digits();
        write!(f, "{}-{}", &digits[..3], &digits[3..])
    }
}

impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(..)")
    }
}

/// How the person at this system takes part in pairing.
pub trait PairingPrompt {
    /// Show `code` so it can be typed on `peer`.
    fn show_code(&mut self, code: &PairingCode, peer: &str);

    /// Ask for the code shown on `peer`.
    fn ask_code(&mut self, peer: &str) -> impl Future<Output = Result<PairingCode>> + Send;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trust {
    /// Both sides already had each other's keys pinned.
    AlreadyPaired,
    /// The two sides paired during this session and pinned each other's keys.
    NewlyPaired,
}

/// Settle whether the peer is trusted, pairing if needed and allowed.
///
/// A peer whose trust has expired counts as unpaired. Pairing pins the peer
/// under `policy`. Returns the peer's name on success. On any failure
/// nothing is pinned and the session must be dropped.
pub async fn establish_trust<S, P>(
    channel: &mut Channel<S>,
    peers: &PeerStore,
    local_name: &str,
    allow_pairing: bool,
    policy: Policy,
    prompt: &mut P,
) -> Result<(String, Trust)>
where
    S: AsyncRead + AsyncWrite + Unpin,
    P: PairingPrompt,
{
    let remote_key = channel.remote_key();
    let known = peers.trusted(&remote_key, trust::now())?.is_some();

    channel
        .send(&Message::Hello {
            name: local_name.to_owned(),
            trusts_you: known,
            will_pair: allow_pairing,
        })
        .await?;
    let (peer_name, peer_knows_us, peer_will_pair) = match channel.recv().await? {
        Message::Hello {
            name,
            trusts_you,
            will_pair,
        } => (name, trusts_you, will_pair),
        other => bail!("expected a hello from the peer, got {other:?}"),
    };
    let peer_name = safe_peer_name(&peer_name);

    if known && peer_knows_us {
        return Ok((peer_name, Trust::AlreadyPaired));
    }
    if !allow_pairing || !peer_will_pair {
        bail!("{peer_name} ({remote_key}) is not paired with this system; pair both systems again");
    }

    let code = match channel.role() {
        Role::Responder => {
            let code = PairingCode::random();
            prompt.show_code(&code, &peer_name);
            code
        }
        Role::Initiator => prompt.ask_code(&peer_name).await?,
    };

    let key = exchange_pairing_key(channel, &code).await?;
    confirm(channel, &key).await?;

    peers.pin(remote_key, &peer_name, policy, trust::now())?;
    Ok((peer_name, Trust::NewlyPaired))
}

fn safe_peer_name(name: &str) -> String {
    let clean = name
        .chars()
        .map(|character| if character.is_control() { ' ' } else { character })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(MAX_PEER_NAME_CHARS)
        .collect::<String>();
    if clean.is_empty() { "Peer".to_owned() } else { clean }
}

async fn exchange_pairing_key<S>(channel: &mut Channel<S>, code: &PairingCode) -> Result<Vec<u8>>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (state, outbound) =
        Spake2::<Ed25519Group>::start_symmetric(&Password::new(code.digits()), &SpakeIdentity::new(SPAKE_IDENTITY));
    channel.send(&Message::PairingKeyExchange { message: outbound }).await?;
    let inbound = match channel.recv().await? {
        Message::PairingKeyExchange { message } => message,
        other => bail!("expected a pairing key exchange, got {other:?}"),
    };
    // a malformed message is an attack or a bug; either way, stop
    state
        .finish(&inbound)
        .map_err(|error| anyhow::anyhow!("pairing key exchange failed: {error:?}"))
}

async fn confirm<S>(channel: &mut Channel<S>, key: &[u8]) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let role = channel.role();
    let peer_role = match role {
        Role::Initiator => Role::Responder,
        Role::Responder => Role::Initiator,
    };

    let tag = confirmation_tag(key, role, channel.handshake_hash());
    channel.send(&Message::PairingConfirmation { tag }).await?;
    let received = match channel.recv().await? {
        Message::PairingConfirmation { tag } => tag,
        other => bail!("expected a pairing confirmation, got {other:?}"),
    };

    let expected = confirmation_tag(key, peer_role, channel.handshake_hash());
    if !bool::from(expected.ct_eq(&received)) {
        bail!("the pairing code did not match, so nothing was paired; try again with the code on screen");
    }
    Ok(())
}

// The role keeps one side's proof from being reflected back as the other's,
// and the handshake hash ties the proof to this session alone.
fn confirmation_tag(key: &[u8], role: Role, handshake_hash: &[u8]) -> [u8; CONFIRMATION_LEN] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC accepts keys of any length");
    mac.update(CONFIRMATION_LABEL);
    mac.update(match role {
        Role::Initiator => b"initiator",
        Role::Responder => b"responder",
    });
    mac.update(handshake_hash);
    mac.finalize().into_bytes().into()
}

/// Reads codes typed into a terminal and prints codes to it.
#[derive(Clone)]
pub struct TerminalPrompt;

impl PairingPrompt for TerminalPrompt {
    fn show_code(&mut self, code: &PairingCode, peer: &str) {
        println!("\nPairing with {peer}. Type this code on {peer}:\n\n    {code}\n");
    }

    fn ask_code(&mut self, peer: &str) -> impl Future<Output = Result<PairingCode>> + Send {
        let peer = peer.to_owned();
        async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

            let mut stdin = BufReader::new(tokio::io::stdin());
            let mut stdout = tokio::io::stdout();
            loop {
                stdout
                    .write_all(format!("Enter the code shown on {peer}: ").as_bytes())
                    .await?;
                stdout.flush().await?;
                let mut line = String::new();
                if stdin.read_line(&mut line).await.context("reading the pairing code")? == 0 {
                    bail!("no pairing code entered");
                }
                match PairingCode::parse(line.trim()) {
                    Some(code) => return Ok(code),
                    None => println!("That is not a 6 digit code."),
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use tokio::io::{DuplexStream, duplex};
    use tokio::sync::oneshot;

    #[test]
    fn peer_names_are_safe_for_terminal_output() {
        assert_eq!(safe_peer_name("\u{1b}[31mBad\nName\r"), "[31mBad Name");
        assert_eq!(safe_peer_name("\0\n\t"), "Peer");
        assert_eq!(safe_peer_name(&"x".repeat(100)).chars().count(), 80);
    }

    enum Answer {
        Fixed(PairingCode),
        FromPeer(oneshot::Receiver<PairingCode>),
    }

    #[derive(Default)]
    struct TestPrompt {
        reveal: Option<oneshot::Sender<PairingCode>>,
        answer: Option<Answer>,
        calls: usize,
    }

    impl PairingPrompt for TestPrompt {
        fn show_code(&mut self, code: &PairingCode, _peer: &str) {
            self.calls += 1;
            if let Some(reveal) = self.reveal.take() {
                let _ = reveal.send(*code);
            }
        }

        fn ask_code(&mut self, _peer: &str) -> impl Future<Output = Result<PairingCode>> + Send {
            self.calls += 1;
            let answer = self.answer.take();
            async move {
                match answer {
                    Some(Answer::Fixed(code)) => Ok(code),
                    Some(Answer::FromPeer(receiver)) => Ok(receiver.await?),
                    None => bail!("no code available"),
                }
            }
        }
    }

    /// A party under test: its key, its paired peers, and the person at it.
    struct Party {
        identity: Identity,
        peers: PeerStore,
        _dir: tempfile::TempDir,
    }

    impl Party {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                identity: Identity::generate().unwrap(),
                peers: PeerStore::open(dir.path()).unwrap(),
                _dir: dir,
            }
        }
    }

    async fn sessions(connecting: &Party, accepting: &Party) -> (Channel<DuplexStream>, Channel<DuplexStream>) {
        let (a, b) = duplex(1 << 17);
        let (left, right) = tokio::join!(
            Channel::initiate(a, &connecting.identity),
            Channel::respond(b, &accepting.identity)
        );
        (left.unwrap(), right.unwrap())
    }

    /// Run trust negotiation on both parties at once, with the typed code
    /// either copied from the other screen or supplied as a wrong guess.
    async fn negotiate(
        connecting: &mut Party,
        accepting: &mut Party,
        allow: (bool, bool),
        typed: Option<PairingCode>,
    ) -> (Result<(String, Trust)>, Result<(String, Trust)>, usize, usize) {
        let (mut left, mut right) = sessions(connecting, accepting).await;
        let (reveal, revealed) = oneshot::channel();
        let mut accepting_prompt = TestPrompt {
            reveal: Some(reveal),
            ..Default::default()
        };
        let mut connecting_prompt = TestPrompt {
            answer: Some(match typed {
                Some(code) => Answer::Fixed(code),
                None => Answer::FromPeer(revealed),
            }),
            ..Default::default()
        };

        let (left_result, right_result) = tokio::join!(
            establish_trust(
                &mut left,
                &connecting.peers,
                "laptop",
                allow.0,
                Policy::default(),
                &mut connecting_prompt
            ),
            establish_trust(
                &mut right,
                &accepting.peers,
                "studio",
                allow.1,
                Policy::default(),
                &mut accepting_prompt
            )
        );
        (
            left_result,
            right_result,
            connecting_prompt.calls,
            accepting_prompt.calls,
        )
    }

    #[tokio::test]
    async fn correct_code_pairs_both_systems() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), None).await;

        assert_eq!(left.unwrap(), ("studio".to_owned(), Trust::NewlyPaired));
        assert_eq!(right.unwrap(), ("laptop".to_owned(), Trust::NewlyPaired));
        assert_eq!(
            laptop
                .peers
                .trusted(&studio.identity.public_key(), trust::now())
                .unwrap()
                .unwrap()
                .name,
            "studio"
        );
        assert_eq!(
            studio
                .peers
                .trusted(&laptop.identity.public_key(), trust::now())
                .unwrap()
                .unwrap()
                .name,
            "laptop"
        );
    }

    #[tokio::test]
    async fn paired_peers_reconnect_without_prompting() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), None).await;
        assert!(left.is_ok() && right.is_ok(), "setup pairing failed");

        let (left, right, left_calls, right_calls) = negotiate(&mut laptop, &mut studio, (false, false), None).await;
        assert_eq!(left.unwrap().1, Trust::AlreadyPaired);
        assert_eq!(right.unwrap().1, Trust::AlreadyPaired);
        assert_eq!((left_calls, right_calls), (0, 0));
    }

    #[tokio::test]
    async fn wrong_code_pairs_nothing() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        let guess = PairingCode::parse("000000").unwrap();
        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), Some(guess)).await;

        // a wrong guess could match the random code one time in a million
        assert!(left.is_err() && right.is_err());
        assert!(
            laptop
                .peers
                .trusted(&studio.identity.public_key(), trust::now())
                .unwrap()
                .is_none()
        );
        assert!(
            studio
                .peers
                .trusted(&laptop.identity.public_key(), trust::now())
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn unknown_peer_is_refused_without_pairing_mode() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        for allow in [(false, true), (true, false), (false, false)] {
            let (left, right, left_calls, right_calls) = negotiate(&mut laptop, &mut studio, allow, None).await;
            assert!(left.unwrap_err().to_string().contains("not paired"));
            assert!(right.unwrap_err().to_string().contains("not paired"));
            assert_eq!(
                (left_calls, right_calls),
                (0, 0),
                "no code should be shown or asked for"
            );
        }
        let now = trust::now();
        assert_eq!(
            laptop.peers.list(now).unwrap().len() + studio.peers.list(now).unwrap().len(),
            0
        );
    }

    #[tokio::test]
    async fn one_sided_forget_requires_pairing_again() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), None).await;
        assert!(left.is_ok() && right.is_ok(), "setup pairing failed");
        studio.peers.forget(&["laptop".to_owned()], trust::now()).unwrap();

        let (left, _, _, _) = negotiate(&mut laptop, &mut studio, (false, false), None).await;
        assert!(left.unwrap_err().to_string().contains("not paired"));

        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), None).await;
        assert_eq!(left.unwrap().1, Trust::NewlyPaired);
        assert_eq!(right.unwrap().1, Trust::NewlyPaired);
    }

    #[tokio::test]
    async fn expired_trust_requires_pairing_again() {
        let (mut laptop, mut studio) = (Party::new(), Party::new());
        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (true, true), None).await;
        assert!(left.is_ok() && right.is_ok(), "setup pairing failed");

        // the laptop still trusts the studio, but the studio last saw the
        // laptop longer ago than its policy allows
        let long_ago = trust::now() - trust::IDLE_LIMIT.as_secs();
        let laptop_key = laptop.identity.public_key();
        studio.peers.pin(laptop_key, "laptop", Policy::IDLE, long_ago).unwrap();

        let (left, right, _, _) = negotiate(&mut laptop, &mut studio, (false, false), None).await;
        assert!(left.unwrap_err().to_string().contains("not paired"));
        assert!(right.unwrap_err().to_string().contains("not paired"));
        assert!(studio.peers.trusted(&laptop_key, trust::now()).unwrap().is_none());
    }

    #[test]
    fn confirmation_is_bound_to_role_and_session() {
        let key = [9; 32];
        let session = [1; 32];
        let other_session = [2; 32];
        let initiator = confirmation_tag(&key, Role::Initiator, &session);
        assert_ne!(initiator, confirmation_tag(&key, Role::Responder, &session));
        assert_ne!(initiator, confirmation_tag(&key, Role::Initiator, &other_session));
        assert_ne!(initiator, confirmation_tag(&[8; 32], Role::Initiator, &session));
        assert_eq!(initiator, confirmation_tag(&key, Role::Initiator, &session));
    }

    #[test]
    fn codes_parse_and_display() {
        let code = PairingCode::parse("048 213").unwrap();
        assert_eq!(code.to_string(), "048-213");
        assert_eq!(PairingCode::parse("048-213"), Some(code));
        assert_eq!(PairingCode::parse("048213"), Some(code));
        for bad in ["", "12345", "1234567", "12a456", "+12345"] {
            assert_eq!(PairingCode::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn random_codes_are_six_digits_and_vary() {
        let codes: std::collections::HashSet<u32> = (0..200).map(|_| PairingCode::random().0).collect();
        assert!(codes.iter().all(|&code| code < CODE_SPACE));
        assert!(codes.len() > 190, "codes repeat too often: {}", codes.len());
    }
}
