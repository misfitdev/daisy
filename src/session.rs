//! Encrypted channel between two systems.
//!
//! Every connection runs a Noise XX handshake: each side learns the other's
//! long-term key and all later traffic is encrypted and authenticated. XX
//! alone does not prove the key belongs to the peer you meant to reach; see
//! `pairing` for how keys become trusted.

use std::io;
use std::sync::Arc;

use snow::{HandshakeState, StatelessTransportState};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};

use crate::identity::{Identity, PublicKey};
use crate::protocol::Message;

/// Noise XX with X25519, ChaCha20-Poly1305 and BLAKE2s; both sides send
/// their long-term keys.
pub(crate) const NOISE_PATTERN: &str = "Noise_XX_25519_ChaChaPoly_BLAKE2s";

/// Bound into every handshake. Never changes: versions are compared through
/// the handshake payloads instead, so a mismatch can be explained.
const PROLOGUE: &[u8] = b"daisy";

/// Session protocol version. Raise it for any change that would make two
/// versions misread each other; sessions need the same version on both sides.
pub const PROTOCOL: u16 = 2;

/// Longest app version carried in a handshake payload.
const MAX_APP_VERSION: usize = 64;

/// What a system runs, sent inside the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub protocol: u16,
    /// The Daisy release, as people see it.
    pub app: String,
}

impl Version {
    pub fn this_system() -> Self {
        Self {
            protocol: PROTOCOL,
            app: env!("CARGO_PKG_VERSION").to_owned(),
        }
    }

    // Every version must read this layout: the protocol as two big-endian
    // bytes, then the app version in UTF-8.
    fn encode(&self) -> Vec<u8> {
        let app = &self.app.as_bytes()[..self.app.len().min(MAX_APP_VERSION)];
        let mut bytes = self.protocol.to_be_bytes().to_vec();
        bytes.extend_from_slice(app);
        bytes
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        let (protocol, app) = bytes.split_first_chunk::<2>()?;
        (app.len() <= MAX_APP_VERSION).then(|| Self {
            protocol: u16::from_be_bytes(*protocol),
            app: String::from_utf8_lossy(app).into_owned(),
        })
    }
}

/// Two systems run different protocol versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    pub peer_key: PublicKey,
    pub peer: Version,
    pub local: Version,
}

impl Mismatch {
    /// Whether this system is the one that needs updating.
    pub fn update_here(&self) -> bool {
        self.local.protocol < self.peer.protocol
    }
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the peer runs Daisy {} (protocol {}) and this system runs Daisy {} (protocol {}); update Daisy on {} to connect",
            self.peer.app,
            self.peer.protocol,
            self.local.app,
            self.local.protocol,
            if self.update_here() { "this system" } else { "the peer" }
        )
    }
}

// Noise caps one message at 65535 bytes, including a 16 byte tag.
const MAX_FRAME: usize = 65_535;
const TAG_LEN: usize = 16;

/// Largest encoded message that fits in one frame.
pub const MAX_MESSAGE: usize = MAX_FRAME - TAG_LEN;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Opened the connection.
    Initiator,
    /// Accepted the connection.
    Responder,
}

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("connection closed by peer")]
    Closed,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("encryption failed: {0}")]
    Noise(#[from] snow::Error),
    #[error("message is {0} bytes, over the {MAX_MESSAGE} byte limit")]
    TooLarge(usize),
    #[error("malformed message: {0}")]
    Malformed(#[from] postcard::Error),
    #[error("peer did not present a key")]
    NoRemoteKey,
    /// The peer predates version reporting, or the handshake was altered.
    #[error("the peer runs a different version of Daisy; update Daisy on both systems")]
    Incompatible,
    #[error("{0}")]
    VersionMismatch(Box<Mismatch>),
}

pub struct Channel<S> {
    stream: S,
    // Explicit nonces let the two directions be used from separate halves.
    // Each counts up from zero, so a replayed or reordered frame fails to
    // authenticate, exactly as with snow's stateful transport.
    transport: Arc<StatelessTransportState>,
    send_nonce: u64,
    recv_nonce: u64,
    role: Role,
    remote_key: PublicKey,
    handshake_hash: Vec<u8>,
    buffer: Vec<u8>,
}

/// Sending half of a split [`Channel`].
pub struct ChannelSender<W> {
    writer: W,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buffer: Vec<u8>,
}

/// Receiving half of a split [`Channel`].
pub struct ChannelReceiver<R> {
    reader: R,
    transport: Arc<StatelessTransportState>,
    nonce: u64,
    buffer: Vec<u8>,
}

impl<S> Channel<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Run the handshake as the side that opened the connection.
    pub async fn initiate(stream: S, identity: &Identity) -> Result<Self, SessionError> {
        Self::initiate_as(stream, identity, &Version::this_system()).await
    }

    /// Run the handshake as the side that accepted the connection.
    pub async fn respond(stream: S, identity: &Identity) -> Result<Self, SessionError> {
        Self::respond_as(stream, identity, &Version::this_system()).await
    }

    // Each side's version rides in its first handshake message. Both are
    // compared only once the handshake has authenticated them, and both
    // sides finish it first, so each can explain the mismatch.
    async fn initiate_as(mut stream: S, identity: &Identity, local: &Version) -> Result<Self, SessionError> {
        let mut handshake = builder(identity)?.build_initiator()?;
        let mut buffer = vec![0; MAX_FRAME];

        // -> e, with this system's version
        let len = handshake.write_message(&local.encode(), &mut buffer)?;
        write_frame(&mut stream, &buffer[..len]).await?;
        // <- e, ee, s, es, with the peer's version
        let frame = read_frame(&mut stream).await?;
        let len = handshake.read_message(&frame, &mut buffer).map_err(incompatible)?;
        let peer = Version::decode(&buffer[..len]);
        // -> s, se
        let len = handshake.write_message(&[], &mut buffer)?;
        write_frame(&mut stream, &buffer[..len]).await?;

        Self::finish(stream, handshake, Role::Initiator, buffer, peer, local)
    }

    async fn respond_as(mut stream: S, identity: &Identity, local: &Version) -> Result<Self, SessionError> {
        let mut handshake = builder(identity)?.build_responder()?;
        let mut buffer = vec![0; MAX_FRAME];

        // -> e, with the peer's version
        let frame = read_frame(&mut stream).await?;
        let len = handshake.read_message(&frame, &mut buffer)?;
        let peer = Version::decode(&buffer[..len]);
        // <- e, ee, s, es, with this system's version
        let len = handshake.write_message(&local.encode(), &mut buffer)?;
        write_frame(&mut stream, &buffer[..len]).await?;
        // -> s, se
        let frame = read_frame(&mut stream).await?;
        handshake.read_message(&frame, &mut buffer).map_err(incompatible)?;

        Self::finish(stream, handshake, Role::Responder, buffer, peer, local)
    }

    fn finish(
        stream: S,
        handshake: HandshakeState,
        role: Role,
        buffer: Vec<u8>,
        peer: Option<Version>,
        local: &Version,
    ) -> Result<Self, SessionError> {
        let remote_key = handshake
            .get_remote_static()
            .and_then(PublicKey::from_bytes)
            .ok_or(SessionError::NoRemoteKey)?;
        let peer = peer.ok_or(SessionError::Incompatible)?;
        if peer.protocol != local.protocol {
            return Err(SessionError::VersionMismatch(Box::new(Mismatch {
                peer_key: remote_key,
                peer,
                local: local.clone(),
            })));
        }
        let handshake_hash = handshake.get_handshake_hash().to_vec();
        let transport = Arc::new(handshake.into_stateless_transport_mode()?);
        Ok(Self {
            stream,
            transport,
            send_nonce: 0,
            recv_nonce: 0,
            role,
            remote_key,
            handshake_hash,
            buffer,
        })
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// The long-term key the peer proved it holds during the handshake.
    pub fn remote_key(&self) -> PublicKey {
        self.remote_key
    }

    /// Identical on both ends of this session and unique to it, so a proof
    /// computed over it cannot be replayed on another connection.
    pub fn handshake_hash(&self) -> &[u8] {
        &self.handshake_hash
    }

    pub async fn send(&mut self, message: &Message) -> Result<(), SessionError> {
        let len = encrypt(&self.transport, &mut self.send_nonce, message, &mut self.buffer)?;
        write_frame(&mut self.stream, &self.buffer[..len]).await
    }

    pub async fn recv(&mut self) -> Result<Message, SessionError> {
        let frame = read_frame(&mut self.stream).await?;
        decrypt(&self.transport, &mut self.recv_nonce, &frame, &mut self.buffer)
    }

    /// Separate the channel into halves that can be used concurrently, so
    /// input can be sent while messages are still being received.
    pub fn split(self) -> (ChannelSender<WriteHalf<S>>, ChannelReceiver<ReadHalf<S>>) {
        let (reader, writer) = tokio::io::split(self.stream);
        let sender = ChannelSender {
            writer,
            transport: self.transport.clone(),
            nonce: self.send_nonce,
            buffer: self.buffer,
        };
        let receiver = ChannelReceiver {
            reader,
            transport: self.transport,
            nonce: self.recv_nonce,
            buffer: vec![0; MAX_FRAME],
        };
        (sender, receiver)
    }
}

impl<W: AsyncWrite + Unpin> ChannelSender<W> {
    pub async fn send(&mut self, message: &Message) -> Result<(), SessionError> {
        let len = encrypt(&self.transport, &mut self.nonce, message, &mut self.buffer)?;
        write_frame(&mut self.writer, &self.buffer[..len]).await
    }
}

impl<R: AsyncRead + Unpin> ChannelReceiver<R> {
    /// Cancel safe only between messages: dropping this future partway
    /// through a frame loses it. Run it in its own task.
    pub async fn recv(&mut self) -> Result<Message, SessionError> {
        let frame = read_frame(&mut self.reader).await?;
        decrypt(&self.transport, &mut self.nonce, &frame, &mut self.buffer)
    }
}

fn encrypt(
    transport: &StatelessTransportState,
    nonce: &mut u64,
    message: &Message,
    buffer: &mut [u8],
) -> Result<usize, SessionError> {
    let plaintext = message.encode()?;
    if plaintext.len() > MAX_MESSAGE {
        return Err(SessionError::TooLarge(plaintext.len()));
    }
    let len = transport.write_message(*nonce, &plaintext, buffer)?;
    *nonce += 1;
    Ok(len)
}

fn decrypt(
    transport: &StatelessTransportState,
    nonce: &mut u64,
    frame: &[u8],
    buffer: &mut [u8],
) -> Result<Message, SessionError> {
    let len = transport.read_message(*nonce, frame, buffer)?;
    *nonce += 1;
    Ok(Message::decode(&buffer[..len])?)
}

fn builder(identity: &Identity) -> Result<snow::Builder<'_>, SessionError> {
    Ok(snow::Builder::new(NOISE_PATTERN.parse()?)
        .local_private_key(identity.private_key())?
        .prologue(PROLOGUE)?)
}

// A different prologue, from a peer that predates version reporting, is the
// only way an unaltered handshake fails to decrypt.
fn incompatible(error: snow::Error) -> SessionError {
    match error {
        snow::Error::Decrypt => SessionError::Incompatible,
        other => other.into(),
    }
}

// Frames are a big-endian u16 length followed by that many bytes.
async fn write_frame<S: AsyncWrite + Unpin>(stream: &mut S, frame: &[u8]) -> Result<(), SessionError> {
    let len = u16::try_from(frame.len()).map_err(|_| SessionError::TooLarge(frame.len()))?;
    stream.write_all(&len.to_be_bytes()).await?;
    stream.write_all(frame).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_frame<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>, SessionError> {
    let mut len = [0; 2];
    read_exact(stream, &mut len).await?;
    let mut frame = vec![0; usize::from(u16::from_be_bytes(len))];
    read_exact(stream, &mut frame).await?;
    Ok(frame)
}

async fn read_exact<S: AsyncRead + Unpin>(stream: &mut S, buffer: &mut [u8]) -> Result<(), SessionError> {
    match stream.read_exact(buffer).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Err(SessionError::Closed),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{DuplexStream, duplex};

    async fn connected_pair() -> (Channel<DuplexStream>, Channel<DuplexStream>, Identity, Identity) {
        let (a, b) = duplex(2 * MAX_FRAME);
        let initiator = Identity::generate().unwrap();
        let responder = Identity::generate().unwrap();
        let (left, right) = tokio::join!(Channel::initiate(a, &initiator), Channel::respond(b, &responder));
        (left.unwrap(), right.unwrap(), initiator, responder)
    }

    #[tokio::test]
    async fn handshake_exchanges_keys_and_agrees_on_hash() {
        let (left, right, initiator, responder) = connected_pair().await;
        assert_eq!(left.remote_key(), responder.public_key());
        assert_eq!(right.remote_key(), initiator.public_key());
        assert_eq!(left.handshake_hash(), right.handshake_hash());
        assert_eq!(left.role(), Role::Initiator);
        assert_eq!(right.role(), Role::Responder);
    }

    fn version(protocol: u16, app: &str) -> Version {
        Version {
            protocol,
            app: app.to_owned(),
        }
    }

    #[tokio::test]
    async fn both_sides_explain_a_protocol_mismatch() {
        let (a, b) = duplex(2 * MAX_FRAME);
        let (older, newer) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (old_release, new_release) = (version(2, "0.1.2"), version(3, "0.1.3"));
        let (left, right) = tokio::join!(
            Channel::initiate_as(a, &older, &old_release),
            Channel::respond_as(b, &newer, &new_release)
        );
        let Err(SessionError::VersionMismatch(left)) = left else {
            panic!("expected a mismatch");
        };
        let Err(SessionError::VersionMismatch(right)) = right else {
            panic!("expected a mismatch");
        };
        assert_eq!(left.peer, version(3, "0.1.3"));
        assert_eq!(left.peer_key, newer.public_key());
        assert!(left.update_here());
        assert_eq!(right.peer, version(2, "0.1.2"));
        assert!(!right.update_here());
        assert_eq!(
            left.to_string(),
            "the peer runs Daisy 0.1.3 (protocol 3) and this system runs Daisy 0.1.2 (protocol 2); update Daisy on this system to connect"
        );
    }

    #[tokio::test]
    async fn different_releases_on_one_protocol_connect() {
        let (a, b) = duplex(2 * MAX_FRAME);
        let (left, right) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (first, second) = (version(PROTOCOL, "0.1.2"), version(PROTOCOL, "0.1.3"));
        let (left, right) = tokio::join!(
            Channel::initiate_as(a, &left, &first),
            Channel::respond_as(b, &right, &second)
        );
        assert!(left.is_ok() && right.is_ok());
    }

    #[test]
    fn a_version_survives_the_wire_and_garbage_does_not() {
        let this = Version::this_system();
        assert_eq!(Version::decode(&this.encode()), Some(this));
        assert_eq!(Version::decode(&[2]), None);
        assert_eq!(Version::decode(&[0; 2 + MAX_APP_VERSION + 1]), None);
    }

    #[tokio::test]
    async fn handshake_hash_differs_per_session() {
        let (first, _, _, _) = connected_pair().await;
        let (second, _, _, _) = connected_pair().await;
        assert_ne!(first.handshake_hash(), second.handshake_hash());
    }

    #[tokio::test]
    async fn messages_arrive_in_order_both_ways() {
        let (mut left, mut right, _, _) = connected_pair().await;
        for nonce in 0..100 {
            left.send(&Message::Ping { nonce }).await.unwrap();
        }
        for nonce in 0..100 {
            assert_eq!(right.recv().await.unwrap(), Message::Ping { nonce });
        }
        right.send(&Message::Pong { nonce: 7 }).await.unwrap();
        assert_eq!(left.recv().await.unwrap(), Message::Pong { nonce: 7 });
    }

    #[tokio::test]
    async fn split_halves_continue_the_session() {
        let (mut left, mut right, _, _) = connected_pair().await;
        // traffic before the split advances the nonces the halves inherit
        left.send(&Message::Ping { nonce: 1 }).await.unwrap();
        assert_eq!(right.recv().await.unwrap(), Message::Ping { nonce: 1 });

        let (mut left_tx, mut left_rx) = left.split();
        let (mut right_tx, mut right_rx) = right.split();
        let both_ways = async {
            for nonce in 2..50 {
                left_tx.send(&Message::Ping { nonce }).await.unwrap();
                right_tx.send(&Message::Pong { nonce }).await.unwrap();
            }
        };
        let receive = async {
            for nonce in 2..50 {
                assert_eq!(right_rx.recv().await.unwrap(), Message::Ping { nonce });
                assert_eq!(left_rx.recv().await.unwrap(), Message::Pong { nonce });
            }
        };
        tokio::join!(both_ways, receive);
    }

    #[tokio::test]
    async fn replayed_frame_is_rejected() {
        // record the first frame on the wire, then deliver it twice
        let (a, b) = duplex(2 * MAX_FRAME);
        let initiator = Identity::generate().unwrap();
        let responder = Identity::generate().unwrap();
        let (left, right) = tokio::join!(Channel::initiate(a, &initiator), Channel::respond(b, &responder));
        let (left, mut right) = (left.unwrap(), right.unwrap());

        let mut buffer = vec![0; MAX_FRAME];
        let mut nonce = left.send_nonce;
        let len = encrypt(&left.transport, &mut nonce, &Message::Ping { nonce: 9 }, &mut buffer).unwrap();
        let frame = buffer[..len].to_vec();

        let mut plain = vec![0; MAX_FRAME];
        assert_eq!(
            decrypt(&right.transport, &mut right.recv_nonce, &frame, &mut plain).unwrap(),
            Message::Ping { nonce: 9 }
        );
        assert!(matches!(
            decrypt(&right.transport, &mut right.recv_nonce, &frame, &mut plain),
            Err(SessionError::Noise(_))
        ));
    }

    #[tokio::test]
    async fn close_is_reported_as_closed() {
        let (mut left, right, _, _) = connected_pair().await;
        drop(right);
        assert!(matches!(left.recv().await, Err(SessionError::Closed)));
    }

    #[tokio::test]
    async fn oversized_message_is_refused() {
        let (mut left, _right, _, _) = connected_pair().await;
        let message = Message::PairingKeyExchange {
            message: vec![0; MAX_MESSAGE + 1],
        };
        assert!(matches!(left.send(&message).await, Err(SessionError::TooLarge(_))));
    }

    // Flips a bit in the next frame written once armed, like an attacker on the wire.
    struct Tamper<S> {
        inner: S,
        armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }

    impl<S: AsyncRead + Unpin> AsyncRead for Tamper<S> {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_read(cx, buf)
        }
    }

    impl<S: AsyncWrite + Unpin> AsyncWrite for Tamper<S> {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            use std::sync::atomic::Ordering;
            // skip the 2 byte length prefix; corrupt the ciphertext itself
            if buf.len() > 2 && self.armed.swap(false, Ordering::SeqCst) {
                let mut forged = buf.to_vec();
                forged[buf.len() / 2] ^= 0x01;
                return std::pin::Pin::new(&mut self.inner).poll_write(cx, &forged);
            }
            std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
        }

        fn poll_flush(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_flush(cx)
        }

        fn poll_shutdown(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    #[tokio::test]
    async fn tampered_ciphertext_is_rejected() {
        let (a, b) = duplex(2 * MAX_FRAME);
        let armed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wire = Tamper {
            inner: a,
            armed: armed.clone(),
        };
        let initiator = Identity::generate().unwrap();
        let responder = Identity::generate().unwrap();
        let (left, right) = tokio::join!(Channel::initiate(wire, &initiator), Channel::respond(b, &responder));
        let (mut left, mut right) = (left.unwrap(), right.unwrap());

        armed.store(true, std::sync::atomic::Ordering::SeqCst);
        left.send(&Message::Ping { nonce: 1 }).await.unwrap();
        assert!(!armed.load(std::sync::atomic::Ordering::SeqCst), "tamper never fired");
        assert!(matches!(right.recv().await, Err(SessionError::Noise(_))));
    }

    #[tokio::test]
    async fn a_peer_on_another_protocol_version_fails_the_handshake() {
        let (a, b) = duplex(2 * MAX_FRAME);
        let initiator = Identity::generate().unwrap();
        let responder = Identity::generate().unwrap();

        // a peer on another protocol version
        let other_version = async move {
            let mut stream = b;
            let mut handshake = snow::Builder::new(NOISE_PATTERN.parse().unwrap())
                .local_private_key(responder.private_key())
                .unwrap()
                .prologue(b"daisy/1")
                .unwrap()
                .build_responder()
                .unwrap();
            let mut buffer = vec![0; MAX_FRAME];
            let frame = read_frame(&mut stream).await.unwrap();
            handshake.read_message(&frame, &mut buffer).unwrap();
            let len = handshake.write_message(&[], &mut buffer).unwrap();
            write_frame(&mut stream, &buffer[..len]).await.unwrap();
            stream
        };

        let (result, _stream) = tokio::join!(Channel::initiate(a, &initiator), other_version);
        assert!(matches!(result, Err(SessionError::Incompatible)), "{:?}", result.err());
    }
}
