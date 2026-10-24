//! Test-only virtual Daisy endpoint. Imported by unit tests with `#[path]`;
//! never included in the application or CLI runtime.

use std::future::Future;

use anyhow::Result;
use tokio::io::DuplexStream;
use tokio::sync::watch;

use crate::device::Signer;
use crate::identity::Identity;
use crate::pairing::{PairingCode, PairingPrompt, Trust, establish_trust};
use crate::peers::PeerStore;
use crate::session::Channel;
use crate::trust::{self, Policy};

struct Endpoint {
    name: String,
    identity: Identity,
    signer: Signer,
    peers: PeerStore,
    _home: tempfile::TempDir,
}

impl Endpoint {
    fn new(name: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        Self {
            name: name.to_owned(),
            identity: Identity::generate().unwrap(),
            signer: Signer::generate().unwrap(),
            peers: PeerStore::open(home.path()).unwrap(),
            _home: home,
        }
    }
}

#[derive(Clone)]
struct CodePrompt {
    shown: Option<watch::Sender<Option<PairingCode>>>,
    received: watch::Receiver<Option<PairingCode>>,
}

impl PairingPrompt for CodePrompt {
    fn show_code(&mut self, code: &PairingCode, _peer: &str) {
        if let Some(shown) = &self.shown {
            shown.send_replace(Some(*code));
        }
    }

    fn ask_code(&mut self, _peer: &str) -> impl Future<Output = Result<PairingCode>> + Send {
        let mut received = self.received.clone();
        async move {
            loop {
                if let Some(code) = *received.borrow_and_update() {
                    return Ok(code);
                }
                received.changed().await?;
            }
        }
    }
}

/// A pair of Daisy sessions with fresh identities and temporary peer stores.
/// Both endpoints pin one another with Daisy's Once policy.
pub async fn paired_channels() -> (Channel<DuplexStream>, Channel<DuplexStream>) {
    let virtual_peer = Endpoint::new("Virtual peer");
    let receiver = Endpoint::new("Validation receiver");
    let (stream_a, stream_b) = tokio::io::duplex(1 << 17);
    let (connect, accept) = tokio::join!(
        Channel::initiate(stream_a, &virtual_peer.identity),
        Channel::respond(stream_b, &receiver.identity),
    );
    let (mut connect, mut accept) = (connect.unwrap(), accept.unwrap());
    let (auth_connect, auth_accept) = tokio::join!(
        connect.authenticate_device(&virtual_peer.signer),
        accept.authenticate_device(&receiver.signer),
    );
    auth_connect.unwrap();
    auth_accept.unwrap();

    let (code_tx, code_rx) = watch::channel(None);
    let mut connect_prompt = CodePrompt {
        shown: None,
        received: code_rx.clone(),
    };
    let mut accept_prompt = CodePrompt {
        shown: Some(code_tx),
        received: code_rx,
    };
    let (connect_trust, accept_trust) = tokio::join!(
        establish_trust(
            &mut connect,
            &virtual_peer.peers,
            &virtual_peer.name,
            true,
            Policy::Once,
            &mut connect_prompt,
        ),
        establish_trust(
            &mut accept,
            &receiver.peers,
            &receiver.name,
            true,
            Policy::Once,
            &mut accept_prompt,
        ),
    );
    assert_eq!(connect_trust.unwrap().1, Trust::NewlyPaired);
    assert_eq!(accept_trust.unwrap().1, Trust::NewlyPaired);
    assert_eq!(
        virtual_peer
            .peers
            .trusted(&receiver.identity.public_key(), trust::now())
            .unwrap()
            .unwrap()
            .policy,
        Policy::Once
    );
    (connect, accept)
}
