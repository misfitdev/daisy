//! Running a sharing session once two Macs trust each other.
//!
//! One Mac drives: it has the keyboard and mouse and sends input across.
//! The other follows, replaying that input. Both keep a heartbeat, so if the
//! other side vanishes, control returns to the driving Mac and anything held
//! down on the following Mac is released.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::{Pin, pin};
use std::time::Duration;

use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::clipboard::{Clipboard, Sharing, WINDOW};
use crate::input::{Action, Along, Rect, Side, Target};
use crate::protocol::{ClipboardPart, Message};
use crate::session::{Channel, ChannelReceiver, ChannelSender, SessionError};

const HEARTBEAT: Duration = Duration::from_secs(1);
/// Silence after which the peer is presumed gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(3);
/// How long the following Mac waits to be told where it sits.
const DRIVE_WAIT: Duration = Duration::from_secs(5);

/// Puts the pointer back on the driving Mac.
pub trait Pointer {
    fn leave(&mut self, along: Along);
}

/// Carries out what the following Mac decided.
pub trait Inject {
    fn execute(&mut self, action: &Action);
}

/// Releases every held key, button, modifier and swipe even if the async
/// follower future is cancelled rather than allowed to return normally.
struct ReleaseOnDrop<'a, I: Inject> {
    target: &'a mut Target,
    injector: &'a mut I,
}

impl<I: Inject> Drop for ReleaseOnDrop<'_, I> {
    fn drop(&mut self) {
        for action in self.target.release_all() {
            self.injector.execute(&action);
        }
    }
}

const INCOMING_CAPACITY: usize = 64;
const OUTGOING_CAPACITY: usize = 64;
const OUTGOING_DRAIN_LIMIT: Duration = Duration::from_secs(1);
/// Clipboard snapshots waiting to be sent. Only the newest waiting one is
/// sent next, so a few crossings in quick succession never pile up.
const CLIPBOARD_CAPACITY: usize = 8;

struct Incoming {
    messages: mpsc::Receiver<Result<Message, SessionError>>,
    task: JoinHandle<()>,
}

impl Incoming {
    async fn recv(&mut self) -> Option<Result<Message, SessionError>> {
        self.messages.recv().await
    }
}

impl Drop for Incoming {
    fn drop(&mut self) {
        self.task.abort();
    }
}

struct Outgoing {
    messages: Option<mpsc::Sender<Message>>,
    clipboard: Option<mpsc::Sender<Vec<ClipboardPart>>>,
    /// Chunks the peer has acknowledged, and so how many more may be sent.
    window: std::sync::Arc<Semaphore>,
    task: JoinHandle<Result<(), SessionError>>,
}

impl Outgoing {
    fn send(&self, message: Message) -> Result<()> {
        let messages = self
            .messages
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("outgoing message queue is closed"))?;
        messages.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                anyhow::anyhow!("outgoing message queue overloaded")
            }
            mpsc::error::TrySendError::Closed(_) => {
                anyhow::anyhow!("outgoing message task stopped")
            }
        })
    }

    /// Reads this Mac's clipboard on a blocking thread and queues it behind
    /// input and heartbeats. Never ends the session: if snapshots back up,
    /// this one is dropped.
    fn send_clipboard(&self, read: Option<impl FnOnce() -> Vec<ClipboardPart> + Send + 'static>) {
        let (Some(read), Some(clipboard)) = (read, self.clipboard.clone()) else {
            return;
        };
        tokio::task::spawn_blocking(move || {
            let parts = read();
            if !parts.is_empty() && clipboard.try_send(parts).is_err() {
                tracing::info!("clipboard queue full; this copy is not shared");
            }
        });
    }

    /// The peer received a chunk; one more may be sent.
    fn acknowledged(&self) {
        if self.window.available_permits() < WINDOW {
            self.window.add_permits(1);
        }
    }

    async fn finished(&mut self) -> Result<()> {
        match (&mut self.task).await {
            Ok(Ok(())) => bail!("outgoing message task stopped"),
            Ok(Err(error)) => Err(error.into()),
            Err(error) => Err(error.into()),
        }
    }

    async fn drain(mut self) -> Result<()> {
        self.messages.take();
        self.clipboard.take();
        match tokio::time::timeout(OUTGOING_DRAIN_LIMIT, &mut self.task).await {
            Ok(Ok(Ok(()))) => Ok(()),
            Ok(Ok(Err(error))) => Err(error.into()),
            Ok(Err(error)) => Err(error.into()),
            Err(_) => bail!("timed out flushing outgoing messages"),
        }
    }
}

impl Drop for Outgoing {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The peer went quiet, rather than closing the session.
#[derive(Debug, thiserror::Error)]
#[error("the peer stopped responding")]
pub struct Silent;

/// Whether a session ended because the connection was lost, as opposed to
/// either system ending it.
pub fn connection_lost(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Silent>().is_some()
        || matches!(error.downcast_ref::<SessionError>(), Some(SessionError::Io(_)))
}

/// Drive the peer, which sits on `side` of this system. `input` carries
/// what the local event tap forwards; the session ends when either side
/// closes or goes silent, or with the error `until` resolves to.
pub async fn drive<S>(
    channel: Channel<S>,
    side: Side,
    mut input: mpsc::Receiver<Message>,
    pointer: &mut impl Pointer,
    sharing: &mut Sharing<impl Clipboard>,
    until: impl Future<Output = anyhow::Error>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut until = pin!(until);
    let (sender, receiver) = channel.split();
    let mut incoming = spawn_receiver(receiver);
    let mut outgoing = spawn_sender(sender);
    outgoing.send(Message::Drive { side })?;

    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut last_heard = Instant::now();
    let mut nonce = 0;
    loop {
        tokio::select! {
            message = input.recv() => match message {
                Some(message) => {
                    // control is crossing to the peer; this Mac's clipboard goes with it
                    let crossing = matches!(message, Message::Enter { .. });
                    // control taken back: the peer's clipboard comes home
                    if matches!(message, Message::Reclaim) {
                        sharing.expect_snapshot();
                    }
                    outgoing.send(message)?;
                    if crossing {
                        outgoing.send_clipboard(sharing.crossing());
                    }
                }
                None => return outgoing.drain().await,
            },
            message = incoming.recv() => {
                let quiet = last_heard.elapsed();
                last_heard = Instant::now();
                match message {
                    Some(Ok(Message::Leave { along })) => {
                        pointer.leave(along);
                        sharing.expect_snapshot();
                    }
                    Some(Ok(Message::Clipboard { part })) => receive_clipboard(part, &outgoing, sharing)?,
                    Some(Ok(Message::Ping { nonce })) => outgoing.send(Message::Pong { nonce })?,
                    Some(Ok(Message::Pong { .. })) => {}
                    Some(Ok(Message::Drive { .. })) => {
                    bail!("both systems are set as Host; choose Host only on the system with the keyboard")
                    }
                Some(Ok(other)) => bail!("unexpected message from the peer: {other:?}"),
                    Some(Err(SessionError::Closed)) | None => return closed_after(quiet),
                    Some(Err(error)) => return Err(error.into()),
                }
            }
            error = &mut until => return Err(error),
            result = outgoing.finished() => return result,
            _ = heartbeat.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    return Err(Silent.into());
                }
                nonce += 1;
                outgoing.send(Message::Ping { nonce })?;
            }
        }
    }
}

/// Follow the peer's lead, replaying its input on `screen`, until the
/// session ends or `until` resolves to an error.
pub async fn follow<S>(
    channel: Channel<S>,
    screen: Rect,
    injector: &mut impl Inject,
    sharing: &mut Sharing<impl Clipboard>,
    until: impl Future<Output = anyhow::Error>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut until = pin!(until);
    let (sender, receiver) = channel.split();
    let mut incoming = spawn_receiver(receiver);
    let mut outgoing = spawn_sender(sender);

    let side = loop {
        let next = tokio::select! {
            next = tokio::time::timeout(DRIVE_WAIT, incoming.recv()) => next,
            error = &mut until => return Err(error),
            result = outgoing.finished() => return result,
        };
        match next {
            Err(_) => bail!("the peer is not set as Host; choose Host on the system with the keyboard"),
            Ok(Some(Ok(Message::Drive { side }))) => break side,
            Ok(Some(Ok(Message::Ping { nonce }))) => outgoing.send(Message::Pong { nonce })?,
            Ok(Some(Ok(other))) => bail!("unexpected message from the peer: {other:?}"),
            Ok(Some(Err(SessionError::Closed)) | None) => return Ok(()),
            Ok(Some(Err(error))) => return Err(error.into()),
        }
    };

    let mut target = Target::new(screen, side);
    let mut release = ReleaseOnDrop {
        target: &mut target,
        injector,
    };
    let ReleaseOnDrop { target, injector } = &mut release;
    replay(&mut outgoing, &mut incoming, target, &mut **injector, sharing, until).await
}

async fn replay(
    outgoing: &mut Outgoing,
    incoming: &mut Incoming,
    target: &mut Target,
    injector: &mut impl Inject,
    sharing: &mut Sharing<impl Clipboard>,
    mut until: Pin<&mut impl Future<Output = anyhow::Error>>,
) -> Result<()> {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut last_heard = Instant::now();
    loop {
        tokio::select! {
            message = incoming.recv() => {
                let quiet = last_heard.elapsed();
                last_heard = Instant::now();
                // control returning to the driving Mac takes this Mac's clipboard back with it
                let mut crossing = false;
                let actions = match message {
                    Some(Ok(Message::Enter { along })) => {
                        sharing.expect_snapshot();
                        target.enter(along)
                    }
                    Some(Ok(Message::Input { event })) => target.input(event),
                    Some(Ok(Message::Reclaim)) => {
                        crossing = true;
                        target.reclaim()
                    }
                    Some(Ok(Message::Clipboard { part })) => {
                        receive_clipboard(part, outgoing, sharing)?;
                        continue;
                    }
                Some(Ok(Message::Ping { nonce })) => {
                    outgoing.send(Message::Pong { nonce })?;
                        continue;
                    }
                    Some(Ok(Message::Pong { .. })) => continue,
            Some(Ok(other)) => bail!("unexpected message from the peer: {other:?}"),
                    Some(Err(SessionError::Closed)) | None => return closed_after(quiet),
                    Some(Err(error)) => return Err(error.into()),
                };
                for action in actions {
                    match action {
                        Action::Leave { along } => {
                            outgoing.send(Message::Leave { along })?;
                            crossing = true;
                        }
                        other => injector.execute(&other),
                    }
                }
                if crossing {
                    outgoing.send_clipboard(sharing.crossing());
                }
            }
            error = &mut until => return Err(error),
            result = outgoing.finished() => return result,
            _ = heartbeat.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT {
                    return Err(Silent.into());
                }
            }
        }
    }
}

/// How a clean close from the peer counts. After a stretch of silence longer
/// than the peer tolerates, the peer closed because it stopped hearing this
/// Mac (sleep, a network change): a lost connection, not a deliberate stop.
fn closed_after(quiet: std::time::Duration) -> Result<()> {
    if quiet > SILENCE_LIMIT {
        Err(Silent.into())
    } else {
        Ok(())
    }
}

/// Handles a clipboard part from the peer: acknowledgements free the send
/// window; every chunk is acknowledged whether or not it is accepted, so the
/// peer's window never stalls.
fn receive_clipboard(part: ClipboardPart, outgoing: &Outgoing, sharing: &mut Sharing<impl Clipboard>) -> Result<()> {
    match part {
        ClipboardPart::Ack => outgoing.acknowledged(),
        ClipboardPart::Chunk { .. } => {
            outgoing.send(Message::Clipboard {
                part: ClipboardPart::Ack,
            })?;
            sharing.receive(part);
        }
        other => sharing.receive(other),
    }
    Ok(())
}

fn spawn_sender<W>(mut sender: ChannelSender<W>) -> Outgoing
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (messages, mut outgoing) = mpsc::channel(OUTGOING_CAPACITY);
    let (clipboard, mut snapshots) = mpsc::channel::<Vec<ClipboardPart>>(CLIPBOARD_CAPACITY);
    let window = std::sync::Arc::new(Semaphore::new(WINDOW));
    let permits = window.clone();
    let task = tokio::spawn(async move {
        let mut snapshots = Some(&mut snapshots);
        let mut pending = VecDeque::new();
        loop {
            // Input and heartbeats first; clipboard parts only when nothing
            // else is waiting. A chunk also needs room in the window, so no
            // more than WINDOW chunks sit in the socket ahead of input.
            let chunk_next = matches!(pending.front(), Some(ClipboardPart::Chunk { .. }));
            tokio::select! {
                biased;
                message = outgoing.recv() => match message {
                    Some(message) => sender.send(&message).await?,
                    None => return Ok(()),
                },
                snapshot = async { snapshots.as_mut().unwrap().recv().await }, if pending.is_empty() && snapshots.is_some() => {
                    match snapshot {
                        Some(mut parts) => {
                            // finish nothing half-sent: take whole snapshots, newest waiting one only
                            while let Ok(newer) = snapshots.as_mut().unwrap().try_recv() {
                                parts = newer;
                            }
                            pending = parts.into();
                        }
                        None => snapshots = None,
                    }
                }
                () = std::future::ready(()), if !pending.is_empty() && !chunk_next => {
                    if let Some(part) = pending.pop_front() {
                        sender.send(&Message::Clipboard { part }).await?;
                    }
                }
                Ok(permit) = permits.acquire(), if chunk_next => {
                    // returned by the peer's acknowledgement, not by this permit
                    permit.forget();
                    if let Some(part) = pending.pop_front() {
                        sender.send(&Message::Clipboard { part }).await?;
                    }
                }
            }
        }
    });
    Outgoing {
        messages: Some(messages),
        clipboard: Some(clipboard),
        window,
        task,
    }
}

// Receiving in its own task keeps frames whole: a select loop that drops a
// half-read frame would corrupt the session.
fn spawn_receiver<R>(mut receiver: ChannelReceiver<R>) -> Incoming
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let (sender, messages) = mpsc::channel(INCOMING_CAPACITY);
    let task = tokio::spawn(async move {
        loop {
            let message = receiver.recv().await;
            let failed = message.is_err();
            if sender.send(message).await.is_err() || failed {
                break;
            }
        }
    });
    Incoming { messages, task }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clipboard::Content;
    use crate::identity::Identity;
    use crate::input::InputEvent;
    use std::future::pending;
    use std::sync::{Arc, Mutex};
    use tokio::io::{DuplexStream, duplex};
    use tokio::sync::watch;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 500.0,
    };

    #[derive(Default, Clone)]
    struct Board {
        count: i64,
        content: Option<Content>,
    }

    impl Clipboard for Board {
        fn change_count(&self) -> i64 {
            self.count
        }
        fn read(&self) -> Option<Content> {
            self.content.clone()
        }
        fn write(&mut self, content: &Content) -> i64 {
            self.count += 1;
            self.content = Some(content.clone());
            self.count
        }
    }

    fn no_clipboard() -> Sharing<Board> {
        Sharing::new(Board::default(), watch::channel(true).1)
    }

    async fn channels() -> (Channel<DuplexStream>, Channel<DuplexStream>) {
        channels_with_capacity(1 << 17).await
    }

    async fn channels_with_capacity(capacity: usize) -> (Channel<DuplexStream>, Channel<DuplexStream>) {
        let (a, b) = duplex(capacity);
        let (left, right) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (left, right) = tokio::join!(Channel::initiate(a, &left), Channel::respond(b, &right));
        (left.unwrap(), right.unwrap())
    }

    #[tokio::test]
    async fn dropping_incoming_aborts_the_receiver_task() {
        let (channel, _peer) = channels().await;
        let (_sender, receiver) = channel.split();
        let incoming = spawn_receiver(receiver);
        let task = incoming.task.abort_handle();

        drop(incoming);
        tokio::time::timeout(Duration::from_secs(1), async {
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("receiver task did not stop after Incoming was dropped");
    }

    #[derive(Default)]
    struct Recorded(Vec<Action>);

    impl Inject for Recorded {
        fn execute(&mut self, action: &Action) {
            self.0.push(action.clone());
        }
    }

    #[derive(Clone, Default)]
    struct SharedRecorded(Arc<Mutex<Vec<Action>>>);

    impl Inject for SharedRecorded {
        fn execute(&mut self, action: &Action) {
            self.0.lock().unwrap().push(action.clone());
        }
    }

    #[derive(Default)]
    struct Returned(Vec<Along>);

    impl Pointer for Returned {
        fn leave(&mut self, along: Along) {
            self.0.push(along);
        }
    }

    #[tokio::test]
    async fn follower_replays_input_and_hands_control_back() {
        let mut board = no_clipboard();
        let (mut driver, follower) = channels().await;
        let mut injector = Recorded::default();

        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            let press = InputEvent::Key {
                code: 0,
                down: true,
                repeat: false,
                flags: 0,
            };
            driver.send(&Message::Input { event: press }).await.unwrap();
            let away = InputEvent::Motion { dx: 5000.0, dy: 0.0 };
            driver.send(&Message::Input { event: away }).await.unwrap();
            let reply = driver.recv().await.unwrap();
            drop(driver);
            reply
        };
        let (reply, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, pending()));

        assert_eq!(reply, Message::Leave { along: 0 });
        result.unwrap();
        assert!(matches!(injector.0[0], Action::Move { .. }), "{:?}", injector.0);
        assert!(injector.0.contains(&Action::Key {
            code: 0,
            down: true,
            repeat: false,
            flags: 0
        }));
        // the held key was released before control went back
        assert!(injector.0.contains(&Action::Key {
            code: 0,
            down: false,
            repeat: false,
            flags: 0
        }));
    }

    #[tokio::test]
    async fn follower_releases_held_input_when_the_driver_vanishes() {
        let mut board = no_clipboard();
        let (mut driver, follower) = channels().await;
        let mut injector = Recorded::default();

        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            let press = InputEvent::Button {
                button: 0,
                down: true,
                clicks: 1,
            };
            driver.send(&Message::Input { event: press }).await.unwrap();
            drop(driver);
        };
        let (_, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, pending()));

        result.unwrap();
        let last = injector.0.last().unwrap();
        assert!(
            matches!(
                last,
                Action::Button {
                    button: 0,
                    down: false,
                    ..
                }
            ),
            "{:?}",
            injector.0
        );
    }

    #[tokio::test(start_paused = true)]
    async fn follower_gives_up_if_nobody_drives() {
        let mut board = no_clipboard();
        let (_driver, follower) = channels().await;
        let error = follow(follower, SCREEN, &mut Recorded::default(), &mut board, pending())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("not set as Host"), "{error}");
    }

    #[tokio::test]
    async fn driver_sends_input_and_takes_control_back() {
        let mut board = no_clipboard();
        let (driver, mut follower) = channels().await;
        let (input, receiver) = mpsc::channel(16);
        let mut pointer = Returned::default();

        input.try_send(Message::Enter { along: 7 }).unwrap();
        let script = async {
            assert_eq!(follower.recv().await.unwrap(), Message::Drive { side: Side::Right });
            // skip heartbeats while looking for the forwarded messages
            let mut seen = Vec::new();
            while seen.is_empty() {
                match follower.recv().await.unwrap() {
                    Message::Ping { .. } => {}
                    other => seen.push(other),
                }
            }
            follower.send(&Message::Leave { along: 9 }).await.unwrap();
            // give the driver a moment to handle it, then hang up
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(follower);
            drop(input);
            seen
        };
        let (seen, result) = tokio::join!(
            script,
            drive(driver, Side::Right, receiver, &mut pointer, &mut board, pending())
        );

        result.unwrap();
        assert_eq!(seen, vec![Message::Enter { along: 7 }]);
        assert_eq!(pointer.0, vec![9]);
    }

    #[tokio::test]
    async fn driver_flushes_accepted_input_when_its_queue_closes() {
        let mut board = no_clipboard();
        let (driver, mut follower) = channels_with_capacity(1).await;
        let (input, events) = mpsc::channel(2);
        input.try_send(Message::Enter { along: 7 }).unwrap();
        drop(input);

        let receive = async {
            assert_eq!(follower.recv().await.unwrap(), Message::Drive { side: Side::Left });
            assert_eq!(follower.recv().await.unwrap(), Message::Enter { along: 7 });
        };
        let mut pointer = Returned::default();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                drive(driver, Side::Left, events, &mut pointer, &mut board, pending()),
                receive
            )
        })
        .await
        .expect("driver did not flush accepted input before closing");

        result.unwrap();
    }

    #[tokio::test]
    async fn two_drivers_are_refused() {
        let mut board1 = no_clipboard();
        let mut board2 = no_clipboard();
        let (left, right) = channels().await;
        let (_left_input, left_events) = mpsc::channel(16);
        let (_right_input, right_events) = mpsc::channel(16);
        let (mut left_pointer, mut right_pointer) = (Returned::default(), Returned::default());
        let (a, b) = tokio::join!(
            drive(left, Side::Left, left_events, &mut left_pointer, &mut board1, pending()),
            drive(
                right,
                Side::Right,
                right_events,
                &mut right_pointer,
                &mut board2,
                pending()
            )
        );
        assert!(a.unwrap_err().to_string().contains("both systems"));
        assert!(b.unwrap_err().to_string().contains("both systems"));
    }

    #[tokio::test(start_paused = true)]
    async fn driver_gives_up_on_a_silent_peer() {
        let mut board = no_clipboard();
        // the peer keeps the connection open but never answers heartbeats
        let (driver, _silent) = channels().await;
        let (_input, events) = mpsc::channel(16);
        let error = drive(
            driver,
            Side::Left,
            events,
            &mut Returned::default(),
            &mut board,
            pending(),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("stopped responding"), "{error}");
        assert!(connection_lost(&error));
    }

    #[tokio::test]
    async fn follower_releases_held_input_when_trust_ends() {
        let mut board = no_clipboard();
        let (mut driver, follower) = channels().await;
        let mut injector = Recorded::default();
        let (revoke, revoked) = tokio::sync::oneshot::channel::<()>();

        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            let press = InputEvent::Button {
                button: 0,
                down: true,
                clicks: 1,
            };
            driver.send(&Message::Input { event: press }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            revoke.send(()).unwrap();
            driver
        };
        let until = async {
            revoked.await.unwrap();
            anyhow::anyhow!("the Host was forgotten")
        };
        let (_driver, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, until));

        let error = result.unwrap_err();
        assert!(error.to_string().contains("forgotten"), "{error}");
        assert!(
            !connection_lost(&error),
            "revoking is deliberate, not a dropped connection"
        );
        assert!(
            matches!(
                injector.0.last(),
                Some(Action::Button {
                    button: 0,
                    down: false,
                    ..
                })
            ),
            "{:?}",
            injector.0
        );
    }

    #[tokio::test]
    async fn cancelling_follower_releases_held_input() {
        let mut board = no_clipboard();
        let (mut driver, follower) = channels().await;
        let recorded = SharedRecorded::default();
        let actions = recorded.0.clone();
        let task = tokio::spawn(async move {
            let mut injector = recorded;
            follow(follower, SCREEN, &mut injector, &mut board, pending()).await
        });

        driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
        driver.send(&Message::Enter { along: 0 }).await.unwrap();
        driver
            .send(&Message::Input {
                event: InputEvent::Button {
                    button: 0,
                    down: true,
                    clicks: 1,
                },
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(matches!(
            actions.lock().unwrap().last(),
            Some(Action::Button {
                button: 0,
                down: false,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn follower_releases_held_input_when_writes_back_up() {
        let mut board = no_clipboard();
        let (mut driver, follower) = channels_with_capacity(1).await;
        let mut injector = Recorded::default();
        let (revoke, revoked) = tokio::sync::oneshot::channel::<()>();

        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            driver
                .send(&Message::Input {
                    event: InputEvent::Button {
                        button: 0,
                        down: true,
                        clicks: 1,
                    },
                })
                .await
                .unwrap();
            driver.send(&Message::Ping { nonce: 1 }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            revoke.send(()).unwrap();
            driver
        };
        let until = async {
            revoked.await.unwrap();
            anyhow::anyhow!("the Host was forgotten")
        };

        let (_driver, result) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, until))
        })
        .await
        .expect("follower did not stop while its socket write was blocked");

        assert!(result.unwrap_err().to_string().contains("forgotten"));
        assert!(matches!(
            injector.0.last(),
            Some(Action::Button {
                button: 0,
                down: false,
                ..
            })
        ));
    }

    #[tokio::test]
    async fn driver_stops_when_trust_ends() {
        let mut board = no_clipboard();
        let (driver, follower) = channels().await;
        let (_input, events) = mpsc::channel(16);
        let until = async { anyhow::anyhow!("the peer was forgotten") };
        let error = drive(driver, Side::Left, events, &mut Returned::default(), &mut board, until)
            .await
            .unwrap_err();
        drop(follower);
        assert!(error.to_string().contains("forgotten"), "{error}");
    }

    #[tokio::test]
    async fn driver_stops_when_trust_ends_while_socket_write_is_blocked() {
        let mut board = no_clipboard();
        let (driver, _peer_that_never_reads) = channels_with_capacity(1).await;
        let (_input, events) = mpsc::channel(16);
        let until = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            anyhow::anyhow!("the peer was forgotten")
        };

        let error = tokio::time::timeout(
            Duration::from_secs(1),
            drive(driver, Side::Left, events, &mut Returned::default(), &mut board, until),
        )
        .await
        .expect("blocked writer prevented trust revocation")
        .unwrap_err();

        assert!(error.to_string().contains("forgotten"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn driver_gives_up_on_a_silent_peer_while_socket_write_is_blocked() {
        let mut board = no_clipboard();
        let (driver, _peer_that_never_reads) = channels_with_capacity(1).await;
        let (_input, events) = mpsc::channel(16);

        let error = tokio::time::timeout(
            Duration::from_secs(5),
            drive(
                driver,
                Side::Left,
                events,
                &mut Returned::default(),
                &mut board,
                pending(),
            ),
        )
        .await
        .expect("blocked writer prevented silence detection")
        .unwrap_err();

        assert!(error.to_string().contains("stopped responding"), "{error}");
    }

    fn board_with(content: Content, on: bool) -> Sharing<Board> {
        let board = Board {
            count: 1,
            content: Some(content),
        };
        Sharing::new(board, watch::channel(on).1)
    }

    fn text(s: &str) -> Content {
        Content {
            text: Some(s.into()),
            ..Content::default()
        }
    }

    /// Messages until the end of the next clipboard snapshot, skipping
    /// heartbeats and acknowledging each chunk as a real peer does.
    async fn until_snapshot_done<S>(peer: &mut Channel<S>) -> Vec<Message>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let mut seen = Vec::new();
        loop {
            match peer.recv().await.unwrap() {
                Message::Ping { .. } => {}
                Message::Clipboard {
                    part: part @ ClipboardPart::Chunk { .. },
                } => {
                    peer.send(&Message::Clipboard {
                        part: ClipboardPart::Ack,
                    })
                    .await
                    .unwrap();
                    seen.push(Message::Clipboard { part });
                }
                Message::Clipboard {
                    part: ClipboardPart::Done,
                } => {
                    seen.push(Message::Clipboard {
                        part: ClipboardPart::Done,
                    });
                    return seen;
                }
                other => seen.push(other),
            }
        }
    }

    fn assembled(messages: &[Message]) -> Option<Content> {
        let mut inbox = crate::clipboard::Inbox::default();
        messages.iter().find_map(|m| match m {
            Message::Clipboard { part } => inbox.accept(part.clone()),
            _ => None,
        })
    }

    #[tokio::test]
    async fn driver_sends_its_clipboard_right_after_crossing() {
        let mut pointer = Returned::default();
        let mut board = board_with(text("copied on the host"), true);
        let (driver, mut follower) = channels().await;
        let (input, receiver) = mpsc::channel(16);
        input.try_send(Message::Enter { along: 7 }).unwrap();
        let script = async {
            assert_eq!(follower.recv().await.unwrap(), Message::Drive { side: Side::Right });
            let seen = until_snapshot_done(&mut follower).await;
            // close the driver's input first so it drains and ends cleanly,
            // then hang up; the other order races a heartbeat onto a closed socket
            drop(input);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(follower);
            seen
        };
        let (seen, result) = tokio::join!(
            script,
            drive(driver, Side::Right, receiver, &mut pointer, &mut board, pending())
        );
        result.unwrap();
        assert_eq!(seen[0], Message::Enter { along: 7 });
        assert_eq!(assembled(&seen), Some(text("copied on the host")));
    }

    #[tokio::test]
    async fn driver_writes_the_clipboard_that_comes_back() {
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let (driver, mut follower) = channels().await;
        let (input, receiver) = mpsc::channel(16);
        let script = async {
            assert_eq!(follower.recv().await.unwrap(), Message::Drive { side: Side::Right });
            // control comes back to the driver, then the guest's clipboard follows
            follower.send(&Message::Leave { along: 3 }).await.unwrap();
            let mut guest = board_with(text("copied on the guest"), true);
            for part in guest.crossing().map(|read| read()).unwrap_or_default() {
                follower.send(&Message::Clipboard { part }).await.unwrap();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            // close the driver's input first so it drains and ends cleanly,
            // then hang up; the other order races a heartbeat onto a closed socket
            drop(input);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(follower);
        };
        let (_, result) = tokio::join!(
            script,
            drive(driver, Side::Right, receiver, &mut pointer, &mut board, pending())
        );
        result.unwrap();
        assert_eq!(board.clipboard().content, Some(text("copied on the guest")));
    }

    #[tokio::test]
    async fn follower_sends_its_clipboard_when_control_leaves() {
        let mut injector = Recorded::default();
        let mut board = board_with(text("copied on the guest"), true);
        let (mut driver, follower) = channels().await;
        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            let away = InputEvent::Motion { dx: 5000.0, dy: 0.0 };
            driver.send(&Message::Input { event: away }).await.unwrap();
            let seen = until_snapshot_done(&mut driver).await;
            drop(driver);
            seen
        };
        let (seen, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, pending()));
        result.unwrap();
        assert_eq!(seen[0], Message::Leave { along: 0 });
        assert_eq!(assembled(&seen), Some(text("copied on the guest")));
    }

    #[tokio::test]
    async fn follower_sends_its_clipboard_when_control_is_reclaimed() {
        let mut injector = Recorded::default();
        let mut board = board_with(text("copied on the guest"), true);
        let (mut driver, follower) = channels().await;
        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            driver.send(&Message::Reclaim).await.unwrap();
            let seen = until_snapshot_done(&mut driver).await;
            drop(driver);
            seen
        };
        let (seen, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, pending()));
        result.unwrap();
        assert_eq!(assembled(&seen), Some(text("copied on the guest")));
    }

    #[tokio::test]
    async fn switched_off_the_follower_sends_and_writes_nothing() {
        let mut injector = Recorded::default();
        let mut board = board_with(text("stays on the guest"), false);
        let (mut driver, follower) = channels().await;
        let script = async {
            driver.send(&Message::Drive { side: Side::Left }).await.unwrap();
            driver.send(&Message::Enter { along: 0 }).await.unwrap();
            let mut host = board_with(text("from the host"), true);
            for part in host.crossing().map(|read| read()).unwrap_or_default() {
                driver.send(&Message::Clipboard { part }).await.unwrap();
            }
            let away = InputEvent::Motion { dx: 5000.0, dy: 0.0 };
            driver.send(&Message::Input { event: away }).await.unwrap();
            let mut reply = driver.recv().await.unwrap();
            while matches!(
                reply,
                Message::Clipboard {
                    part: ClipboardPart::Ack
                }
            ) {
                reply = driver.recv().await.unwrap();
            }
            assert_eq!(reply, Message::Leave { along: 0 });
            let next = tokio::time::timeout(Duration::from_millis(200), driver.recv()).await;
            drop(driver);
            next.is_err()
        };
        let (quiet, result) = tokio::join!(script, follow(follower, SCREEN, &mut injector, &mut board, pending()));
        result.unwrap();
        assert!(quiet, "the follower sent something after Leave");
        assert_eq!(board.clipboard().content, Some(text("stays on the guest")));
    }

    #[tokio::test]
    async fn a_large_image_neither_overloads_the_session_nor_holds_input_back() {
        let mut pointer = Returned::default();
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 70]),
            ..Content::default()
        };
        let mut board = board_with(image.clone(), true);
        let (driver, mut follower) = channels().await;
        let (input, receiver) = mpsc::channel(16);
        input.try_send(Message::Enter { along: 7 }).unwrap();
        let script = async {
            assert_eq!(follower.recv().await.unwrap(), Message::Drive { side: Side::Right });
            let mut seen = Vec::new();
            let mut sent_motion = false;
            loop {
                let message = follower.recv().await.unwrap();
                if matches!(
                    message,
                    Message::Clipboard {
                        part: ClipboardPart::Chunk { .. }
                    }
                ) {
                    follower
                        .send(&Message::Clipboard {
                            part: ClipboardPart::Ack,
                        })
                        .await
                        .unwrap();
                }
                if !sent_motion
                    && matches!(
                        message,
                        Message::Clipboard {
                            part: ClipboardPart::Chunk { .. }
                        }
                    )
                {
                    input
                        .try_send(Message::Input {
                            event: InputEvent::Motion { dx: 1.0, dy: 0.0 },
                        })
                        .unwrap();
                    sent_motion = true;
                }
                let done = matches!(
                    message,
                    Message::Clipboard {
                        part: ClipboardPart::Done
                    }
                );
                if !matches!(message, Message::Ping { .. }) {
                    seen.push(message);
                }
                if done {
                    break;
                }
            }
            // close the driver's input first so it drains and ends cleanly,
            // then hang up; the other order races a heartbeat onto a closed socket
            drop(input);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(follower);
            seen
        };
        let (seen, result) = tokio::join!(
            script,
            drive(driver, Side::Right, receiver, &mut pointer, &mut board, pending())
        );
        result.unwrap();
        assert_eq!(assembled(&seen), Some(image));
        let motion = seen.iter().position(|m| matches!(m, Message::Input { .. })).unwrap();
        assert!(motion < seen.len() - 2, "input waited for the whole image");
    }

    #[tokio::test]
    async fn a_peer_that_does_not_acknowledge_gets_no_more_than_the_window() {
        let mut pointer = Returned::default();
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 50]),
            ..Content::default()
        };
        let mut board = board_with(image, true);
        let (driver, mut follower) = channels().await;
        let (input, receiver) = mpsc::channel(16);
        input.try_send(Message::Enter { along: 7 }).unwrap();
        let script = async {
            let mut chunks = 0;
            // read everything the driver sends for a while, acknowledging nothing
            let _ = tokio::time::timeout(Duration::from_millis(400), async {
                loop {
                    if let Message::Clipboard {
                        part: ClipboardPart::Chunk { .. },
                    } = follower.recv().await.unwrap()
                    {
                        chunks += 1;
                    }
                }
            })
            .await;
            drop(input);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(follower);
            chunks
        };
        let (chunks, _) = tokio::join!(
            script,
            drive(driver, Side::Right, receiver, &mut pointer, &mut board, pending())
        );
        assert_eq!(chunks, crate::clipboard::WINDOW);
    }

    #[test]
    fn a_close_after_silence_counts_as_a_lost_connection() {
        assert!(closed_after(Duration::from_millis(200)).is_ok());
        let lost = closed_after(SILENCE_LIMIT + Duration::from_millis(1)).unwrap_err();
        assert!(connection_lost(&lost));
    }
}
