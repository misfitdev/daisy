//! Exercises live activation and encrypted collection without native input.
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use daisy::clipboard::{Clipboard, Content, Sharing};
use daisy::control::SharedControl;
use daisy::diagnostics::{Record, TraceLayer};
use daisy::identity::Identity;
use daisy::input::{Action, Point, Rect, Side};
use daisy::protocol::Message;
use daisy::session::Channel;
use daisy::share::{Arranging, Inject, Pointer, SharedLayout};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};
use tracing_subscriber::{Layer, layer::SubscriberExt, util::SubscriberInitExt};

struct NoPointer;
impl Pointer for NoPointer {
    fn leave(&mut self, _: Option<Point>) {}
}
struct NoInput;
impl Inject for NoInput {
    fn execute(&mut self, _: &Action) {}
}
#[derive(Clone)]
struct NoClipboard;
impl Clipboard for NoClipboard {
    fn change_count(&self) -> i64 {
        0
    }
    fn read(&self) -> Option<Content> {
        None
    }
    fn write(&mut self, _: &Content) -> i64 {
        0
    }
}

async fn next_trace_message(peer: &mut Channel<tokio::io::DuplexStream>) -> Message {
    loop {
        match peer.recv().await.unwrap() {
            Message::Ping { nonce } => peer.send(&Message::Pong { nonce }).await.unwrap(),
            message @ (Message::TraceControl { .. } | Message::TraceRecord { .. } | Message::TraceAck { .. }) => {
                return message;
            }
            _ => {}
        }
    }
}

#[tokio::test]
async fn one_collector_enables_and_collects_peer_traces_then_disables_them() {
    tokio::time::timeout(Duration::from_secs(5), async {
        let hub = daisy::diagnostics::hub();
        tracing_subscriber::registry()
            .with(
                TraceLayer(hub.clone()).with_filter(tracing_subscriber::filter::dynamic_filter_fn(|meta, _| {
                    daisy::diagnostics::trace_filter(meta)
                })),
            )
            .try_init()
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let local = Identity::generate().unwrap();
        let remote = Identity::generate().unwrap();
        let server = daisy::diagnostics::serve(directory.path(), local.public_key())
            .await
            .unwrap();
        let (a, b) = tokio::io::duplex(1 << 17);
        let (a, b) = tokio::join!(Channel::initiate(a, &local), Channel::respond(b, &remote));
        let channel = a.unwrap();
        let mut peer = b.unwrap();
        assert!(channel.trace_capable());
        let (input_tx, input) = mpsc::channel(16);
        let (stop, stopped) = oneshot::channel();
        let me = local.public_key();
        let session = tokio::spawn(async move {
            daisy::share::together(
                channel,
                SharedLayout {
                    screen: Rect {
                        x: 0.0,
                        y: 0.0,
                        width: 100.0,
                        height: 100.0,
                    },
                    side: Side::Right,
                    control: Arc::new(SharedControl::new(me, me)),
                    arranging: Arranging::fixed((Side::Right, 0), true),
                },
                input,
                &mut NoPointer,
                &mut NoInput,
                &mut Sharing::new(NoClipboard, watch::channel(false).1),
                async {
                    let _ = stopped.await;
                    anyhow::anyhow!("test finished")
                },
            )
            .await
        });
        let mut socket = UnixStream::connect(directory.path().join("developer-trace.sock"))
            .await
            .unwrap();
        socket.write_all(&[1]).await.unwrap();
        let mut collector = BufReader::new(socket);
        let mut line = String::new();
        collector.read_line(&mut line).await.unwrap();
        assert!(line.contains(&local.public_key().to_hex()));
        assert_eq!(
            next_trace_message(&mut peer).await,
            Message::TraceControl { enabled: true }
        );
        let record = Record {
            sequence: 789,
            unix_ms: 12,
            elapsed_ms: 2,
            build: "test".into(),
            level: "TRACE".into(),
            target: "daisy::test".into(),
            dropped_before: 0,
            fields: BTreeMap::from([
                ("message".into(), "peer marker".into()),
                ("system".into(), "forged".into()),
            ]),
        };
        peer.send(&Message::TraceRecord { record }).await.unwrap();
        assert_eq!(next_trace_message(&mut peer).await, Message::TraceAck { sequence: 789 });
        loop {
            line.clear();
            collector.read_line(&mut line).await.unwrap();
            let received: serde_json::Value = serde_json::from_str(&line).unwrap();
            if received["record"]["sequence"] == 789 {
                assert_eq!(received["system"], remote.public_key().to_hex());
                assert_eq!(received["record"]["fields"]["message"], "peer marker");
                break;
            }
        }
        peer.send(&Message::TraceControl { enabled: true }).await.unwrap();
        // Wait for evidence that the peer request reached the session task.
        loop {
            if let Message::TraceRecord { record } = next_trace_message(&mut peer).await {
                peer.send(&Message::TraceAck {
                    sequence: record.sequence,
                })
                .await
                .unwrap();
                if record
                    .fields
                    .get("message")
                    .is_some_and(|s| s.contains("peer developer trace request changed"))
                {
                    break;
                }
            }
        }
        tracing::trace!(target: "daisy::test", "local trace marker");
        loop {
            if let Message::TraceRecord { record } = next_trace_message(&mut peer).await {
                peer.send(&Message::TraceAck {
                    sequence: record.sequence,
                })
                .await
                .unwrap();
                if record
                    .fields
                    .get("message")
                    .is_some_and(|s| s.contains("local trace marker"))
                {
                    break;
                }
            }
        }
        drop(collector);
        loop {
            match next_trace_message(&mut peer).await {
                Message::TraceControl { enabled: false } => break,
                Message::TraceRecord { record } => peer
                    .send(&Message::TraceAck {
                        sequence: record.sequence,
                    })
                    .await
                    .unwrap(),
                _ => {}
            }
        }
        stop.send(()).unwrap();
        assert!(session.await.unwrap().is_err());
        drop(input_tx);
        assert!(
            !hub.enabled(),
            "remote trace leases must end when their authenticated link ends"
        );
        drop(server);
    })
    .await
    .unwrap();
}
