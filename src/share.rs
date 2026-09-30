//! Running a sharing session once two systems trust each other.
//!
//! Whichever system is in use has control and sends its input across when the
//! pointer crosses; the other replays it. Both keep a heartbeat, so if the
//! other side vanishes, anything held down on the replaying system is released.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::pin;
use std::time::Duration;

use anyhow::{Context, Result, bail};
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

/// Puts the pointer back on this system when control returns to it.
pub trait Pointer {
    fn leave(&mut self, along: Along);
    fn yield_control(&mut self) {}
}

pub struct SharedLayout {
    pub screen: Rect,
    pub side: Side,
    pub control: std::sync::Arc<crate::control::SharedControl>,
}

/// Both sides capture physical input and either may take control. Generation
/// stamps exclude queued events from the previous owner after a handoff.
pub async fn together<S>(
    channel: Channel<S>,
    layout: SharedLayout,
    mut input: mpsc::Receiver<Message>,
    pointer: &mut impl Pointer,
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
    let mut target = Target::new(layout.screen, layout.side.opposite());
    let release = ReleaseOnDrop {
        target: &mut target,
        injector,
    };
    let mut wakeups = layout
        .control
        .take_wakeups()
        .context("the input control is already in use by another session")?;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut meter = crate::latency::Meter::default();
    let mut last_heard = Instant::now();
    let mut nonce = 0;
    loop {
        tokio::select! {
            _ = wakeups.recv() => {
                let mut state = layout.control.state.lock().unwrap_or_else(|e| e.into_inner());
                let interrupted = layout.control.interrupted.swap(false, std::sync::atomic::Ordering::AcqRel);
                let claim = if interrupted { Some(state.interrupt(layout.control.now())) } else { None };
                drop(state);
                for action in release.target.reclaim() { release.injector.execute(&action); }
                if interrupted { pointer.yield_control(); }
                if let Some(generation) = claim {
                    outgoing.send(Message::ControlClaim { generation })?;
                    sharing.expect_snapshot();
                }
                layout.control.publish(meter.average());
            }
            message = input.recv() => {
                let Some(message) = message else { return outgoing.drain().await; };
                if matches!(message, Message::ControlClaim { .. }) {
                    for action in release.target.reclaim() { release.injector.execute(&action); }
                    sharing.expect_snapshot();
                }
                let crossing = matches!(message, Message::SharedEnter { .. });
                outgoing.send(message)?;
                if crossing { outgoing.send_clipboard(sharing.crossing()); }
            }
            message = incoming.recv() => {
                let quiet = last_heard.elapsed();
                last_heard = Instant::now();
                let message = match message {
                    Some(Ok(message)) => message,
                    Some(Err(SessionError::Closed)) | None => return closed_after(quiet),
                    Some(Err(error)) => return Err(error.into()),
                };
                match message {
                    Message::Ping { nonce } => outgoing.send(Message::Pong { nonce })?,
                    Message::Pong { nonce } => {
                        if let Some(round_trip) = meter.answered(nonce, layout.control.now())
                            && round_trip > crate::latency::SPIKE
                        {
                            tracing::warn!(ms = round_trip.as_millis(), "slow round trip to the peer");
                        }
                    }
                    Message::Clipboard { part } => receive_clipboard(part, &outgoing, sharing)?,
                    Message::ControlClaim { generation } => {
                        let mut state = layout.control.state.lock().unwrap_or_else(|e| e.into_inner());
                        let changed = state.claim(generation);
                        drop(state);
                        if changed {
                            pointer.yield_control();
                            for action in release.target.reclaim() { release.injector.execute(&action); }
                            outgoing.send_clipboard(sharing.crossing());
                            layout.control.publish(meter.average());
                        }
                    }
                    Message::SharedLeave { generation, along } => {
                        let state = layout.control.state.lock().unwrap_or_else(|e| e.into_inner());
                        let current = state.owns() && state.generation() == generation;
                        drop(state);
                        if current {
                            pointer.leave(along);
                            sharing.expect_snapshot();
                        }
                    }
                    Message::SharedEnter { generation, .. }
                    | Message::SharedInput { generation, .. }
                    | Message::SharedReclaim { generation } => {
                        let state = layout.control.state.lock().unwrap_or_else(|e| e.into_inner());
                        let receives = state.receives(generation, layout.control.now()) && !layout.control.local_busy();
                        drop(state);
                        if !receives {
                            if let Message::SharedEnter { along, .. } = message {
                                outgoing.send(Message::SharedLeave { generation, along })?;
                            }
                            continue;
                        }
                        let mut crossing = false;
                        let actions = match message {
                            Message::SharedEnter { along, .. } => { sharing.expect_snapshot(); release.target.enter(along) },
                            Message::SharedInput { event, .. } => release.target.input(event),
                            Message::SharedReclaim { .. } => { crossing = true; release.target.reclaim() },
                            _ => unreachable!(),
                        };
                        for action in actions {
                            match action {
                                Action::Leave { along } => { outgoing.send(Message::SharedLeave { generation, along })?; crossing = true; },
                                other => release.injector.execute(&other),
                            }
                        }
                        if crossing { outgoing.send_clipboard(sharing.crossing()); }
                    }
                    other => bail!("unexpected message in shared session: {other:?}"),
                }
            }
            error = &mut until => return Err(error),
            result = outgoing.finished() => return result,
            _ = heartbeat.tick() => {
                if last_heard.elapsed() > SILENCE_LIMIT { return Err(Silent.into()); }
                layout.control.publish(meter.average());
                nonce += 1;
                meter.sent(nonce, layout.control.now());
                outgoing.send(Message::Ping { nonce })?;
            }
        }
    }
}

/// Carries out the peer's input on this system.
pub trait Inject {
    fn execute(&mut self, action: &Action);
}

/// Releases every held key, button, modifier and swipe even if the session
/// future is cancelled rather than allowed to return normally.
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

    /// Reads this system's clipboard on a blocking thread and queues it behind
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

/// How a clean close from the peer counts. After a stretch of silence longer
/// than the peer tolerates, the peer closed because it stopped hearing this
/// system (sleep, a network change): a lost connection, not a deliberate stop.
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
    use crate::control::SharedControl;
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

    /// A clipboard every clone shares, like the system one.
    #[derive(Default, Clone)]
    struct Board(Arc<Mutex<(i64, Option<Content>)>>);

    impl Board {
        fn copy(&self, content: Content) -> i64 {
            let mut board = self.0.lock().unwrap();
            board.0 += 1;
            board.1 = Some(content);
            board.0
        }
        fn content(&self) -> Option<Content> {
            self.0.lock().unwrap().1.clone()
        }
    }

    impl Clipboard for Board {
        fn change_count(&self) -> i64 {
            self.0.lock().unwrap().0
        }
        fn read(&self) -> Option<Content> {
            self.content()
        }
        fn write(&mut self, content: &Content) -> i64 {
            self.copy(content.clone())
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
    async fn the_session_reports_latency_and_who_has_control() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(crate::control::SharedControl::new(true));
        let mut link = control.watch_link();
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let answer = async {
            loop {
                if let Message::Ping { nonce } = peer.recv().await.unwrap() {
                    peer.send(&Message::Pong { nonce }).await.unwrap();
                }
            }
        };
        let measured = async {
            loop {
                link.changed().await.unwrap();
                let current = *link.borrow_and_update();
                if current.latency_ms.is_some() {
                    break current;
                }
            }
        };
        let session = together(
            local,
            SharedLayout {
                screen: SCREEN,
                side: Side::Left,
                control: control.clone(),
            },
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let reported = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                reported = measured => reported,
                () = answer => unreachable!(),
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert!(reported.in_control);
        assert!(reported.latency_ms.is_some_and(|ms| ms < 1000));
    }

    #[tokio::test]
    async fn giving_up_control_sends_the_clipboard() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut pointer = Returned::default();
        let mut board = board_with(text("copied before walking away"), true);
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            until_snapshot_done(&mut peer).await
        };
        let session = together(
            local,
            SharedLayout {
                screen: SCREEN,
                side: Side::Left,
                control: control.clone(),
            },
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let seen = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                seen = script => seen,
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(assembled(&seen), Some(text("copied before walking away")));
    }

    #[tokio::test]
    async fn a_busy_system_sends_the_pointer_back_where_it_crossed() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer.send(&Message::Ping { nonce: 1 }).await.unwrap();
            while peer.recv().await.unwrap() != (Message::Pong { nonce: 1 }) {}
            control.note_physical();
            peer.send(&Message::SharedEnter {
                generation: 1,
                along: 123,
            })
            .await
            .unwrap();
            loop {
                match peer.recv().await.unwrap() {
                    Message::SharedLeave { generation, along } => break (generation, along),
                    _ => continue,
                }
            }
        };
        let session = together(
            local,
            SharedLayout {
                screen: SCREEN,
                side: Side::Left,
                control: control.clone(),
            },
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let returned = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                returned = script => returned,
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(returned, (1, 123));
    }

    #[tokio::test]
    async fn shared_handoff_releases_keys_and_rejects_queued_input() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (capture, input) = mpsc::channel(16);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let mut injector = SharedRecorded(actions.clone());
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer.send(&Message::SharedEnter {
                generation: 1,
                along: 0,
            })
            .await
            .unwrap();
            peer.send(&Message::SharedInput {
                generation: 1,
                event: InputEvent::Key {
                    code: 7,
                    down: true,
                    repeat: false,
                    flags: 0,
                },
            })
            .await
            .unwrap();
            peer.send(&Message::Ping { nonce: 99 }).await.unwrap();
            while peer.recv().await.unwrap() != (Message::Pong { nonce: 99 }) {}
            assert!(actions.lock().unwrap().iter().any(|a| matches!(
                a,
                Action::Key {
                    code: 7,
                    down: true,
                    ..
                }
            )));
            let generation = control.state.lock().unwrap().physical(control.now()).unwrap();
            capture.send(Message::ControlClaim { generation }).await.unwrap();
            while peer.recv().await.unwrap() != (Message::ControlClaim { generation }) {}
            assert!(actions.lock().unwrap().iter().any(|a| matches!(
                a,
                Action::Key {
                    code: 7,
                    down: false,
                    ..
                }
            )));
            peer.send(&Message::ControlClaim { generation: 3 }).await.unwrap();
            tokio::time::sleep(crate::control::SETTLE + Duration::from_millis(20)).await;
            peer.send(&Message::SharedEnter {
                generation: 3,
                along: 0,
            })
            .await
            .unwrap();
            peer.send(&Message::SharedInput {
                generation: 1,
                event: InputEvent::Key {
                    code: 8,
                    down: true,
                    repeat: false,
                    flags: 0,
                },
            })
            .await
            .unwrap();
            peer.send(&Message::SharedInput {
                generation: 3,
                event: InputEvent::Key {
                    code: 9,
                    down: true,
                    repeat: false,
                    flags: 0,
                },
            })
            .await
            .unwrap();
            peer.send(&Message::Ping { nonce: 100 }).await.unwrap();
            while peer.recv().await.unwrap() != (Message::Pong { nonce: 100 }) {}
            drop(peer);
        };
        let session = together(
            local,
            SharedLayout {
                screen: SCREEN,
                side: Side::Left,
                control: control.clone(),
            },
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (_, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();
        result.unwrap();
        let actions = actions.lock().unwrap();
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Key {
                code: 7,
                down: false,
                ..
            }
        )));
        assert!(!actions.iter().any(|a| matches!(a, Action::Key { code: 8, .. })));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Key {
                code: 9,
                down: true,
                ..
            }
        )));
    }

    fn layout(control: &Arc<SharedControl>) -> SharedLayout {
        SharedLayout {
            screen: SCREEN,
            side: Side::Left,
            control: control.clone(),
        }
    }

    const PRESS_KEY: InputEvent = InputEvent::Key {
        code: 0,
        down: true,
        repeat: false,
        flags: 0,
    };
    const PRESS_BUTTON: InputEvent = InputEvent::Button {
        button: 0,
        down: true,
        clicks: 1,
    };
    /// Far past the edge the peer sits beyond, so the pointer leaves.
    const AWAY: InputEvent = InputEvent::Motion { dx: -5000.0, dy: 0.0 };

    fn button_released(action: Option<&Action>) -> bool {
        matches!(
            action,
            Some(Action::Button {
                button: 0,
                down: false,
                ..
            })
        )
    }

    /// The peer takes control and moves its pointer onto this system.
    async fn peer_enters<S>(peer: &mut Channel<S>)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
        peer.send(&Message::SharedEnter {
            generation: 1,
            along: 0,
        })
        .await
        .unwrap();
    }

    async fn peer_sends<S>(peer: &mut Channel<S>, event: InputEvent)
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        peer.send(&Message::SharedInput { generation: 1, event }).await.unwrap();
    }

    /// Everything this system sent until it answers `nonce`, so all earlier
    /// messages from the peer have been handled. Heartbeats are skipped.
    async fn round_trip<S>(peer: &mut Channel<S>, nonce: u64) -> Vec<Message>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        peer.send(&Message::Ping { nonce }).await.unwrap();
        let mut seen = Vec::new();
        loop {
            match peer.recv().await.unwrap() {
                Message::Pong { nonce: n } if n == nonce => return seen,
                Message::Ping { .. } => {}
                other => seen.push(other),
            }
        }
    }

    #[tokio::test]
    async fn replays_input_and_hands_control_back() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let script = async {
            peer_enters(&mut peer).await;
            peer_sends(&mut peer, PRESS_KEY).await;
            peer_sends(&mut peer, AWAY).await;
            let seen = round_trip(&mut peer, 1).await;
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen
        };
        let mut pointer = Returned::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert!(
            seen.contains(&Message::SharedLeave {
                generation: 1,
                along: 0
            }),
            "{seen:?}"
        );
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
    async fn releases_held_input_when_the_peer_vanishes() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let script = async {
            peer_enters(&mut peer).await;
            peer_sends(&mut peer, PRESS_BUTTON).await;
            round_trip(&mut peer, 1).await;
            drop(peer);
        };
        let mut pointer = Returned::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (_, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        if let Err(error) = result {
            assert!(connection_lost(&error), "{error}");
        }
        assert!(button_released(injector.0.last()), "{:?}", injector.0);
    }

    #[tokio::test]
    async fn sends_input_and_takes_control_back() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(16);
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        capture
            .try_send(Message::SharedEnter {
                generation: 0,
                along: 7,
            })
            .unwrap();
        let script = async {
            let mut seen = Vec::new();
            while seen.is_empty() {
                match peer.recv().await.unwrap() {
                    Message::Ping { .. } => {}
                    other => seen.push(other),
                }
            }
            peer.send(&Message::SharedLeave {
                generation: 0,
                along: 9,
            })
            .await
            .unwrap();
            round_trip(&mut peer, 1).await;
            // close the input queue first so the session drains and ends
            // cleanly; hanging up first races a heartbeat onto a closed socket
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen
        };
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert_eq!(
            seen,
            vec![Message::SharedEnter {
                generation: 0,
                along: 7
            }]
        );
        assert_eq!(pointer.0, vec![9]);
    }

    #[tokio::test]
    async fn flushes_accepted_input_when_its_queue_closes() {
        let (local, mut peer) = channels_with_capacity(1).await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(2);
        let enter = Message::SharedEnter {
            generation: 0,
            along: 7,
        };
        capture.try_send(enter.clone()).unwrap();
        drop(capture);
        let receive = async {
            loop {
                match peer.recv().await.unwrap() {
                    Message::Ping { .. } => {}
                    other => break other,
                }
            }
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (result, received) = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(session, receive) })
            .await
            .expect("the session did not flush accepted input before closing");

        result.unwrap();
        assert_eq!(received, enter);
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_on_a_silent_peer() {
        // the peer keeps the connection open but never answers heartbeats
        let (local, _silent) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (_capture, input) = mpsc::channel(16);
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let error = tokio::time::timeout(Duration::from_secs(10), session)
            .await
            .expect("a silent peer kept the session open")
            .unwrap_err();
        assert!(error.to_string().contains("stopped responding"), "{error}");
        assert!(connection_lost(&error));
    }

    #[tokio::test]
    async fn releases_held_input_when_trust_ends() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let (revoke, revoked) = tokio::sync::oneshot::channel::<()>();
        let script = async {
            peer_enters(&mut peer).await;
            peer_sends(&mut peer, PRESS_BUTTON).await;
            round_trip(&mut peer, 1).await;
            revoke.send(()).unwrap();
            peer
        };
        let until = async {
            revoked.await.unwrap();
            anyhow::anyhow!("the peer was forgotten")
        };
        let mut pointer = Returned::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            until,
        );
        let (_peer, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        let error = result.unwrap_err();
        assert!(error.to_string().contains("forgotten"), "{error}");
        assert!(
            !connection_lost(&error),
            "revoking is deliberate, not a dropped connection"
        );
        assert!(button_released(injector.0.last()), "{:?}", injector.0);
    }

    #[tokio::test]
    async fn cancelling_the_session_releases_held_input() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let recorded = SharedRecorded::default();
        let actions = recorded.0.clone();
        let task = tokio::spawn(async move {
            let (_capture, input) = mpsc::channel(16);
            let mut injector = recorded;
            together(
                local,
                layout(&control),
                input,
                &mut Returned::default(),
                &mut injector,
                &mut no_clipboard(),
                pending(),
            )
            .await
        });

        peer_enters(&mut peer).await;
        peer_sends(&mut peer, PRESS_BUTTON).await;
        round_trip(&mut peer, 1).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(button_released(actions.lock().unwrap().last()));
    }

    #[tokio::test]
    async fn releases_held_input_when_trust_ends_while_writes_back_up() {
        let (local, mut peer) = channels_with_capacity(1).await;
        let control = Arc::new(SharedControl::new(false));
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let (revoke, revoked) = tokio::sync::oneshot::channel::<()>();
        let script = async {
            peer_enters(&mut peer).await;
            peer_sends(&mut peer, PRESS_BUTTON).await;
            // the answer never gets read, so this system's writes back up
            peer.send(&Message::Ping { nonce: 1 }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            revoke.send(()).unwrap();
            peer
        };
        let until = async {
            revoked.await.unwrap();
            anyhow::anyhow!("the peer was forgotten")
        };
        let mut pointer = Returned::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            until,
        );
        let (_peer, result) = tokio::time::timeout(Duration::from_secs(1), async { tokio::join!(script, session) })
            .await
            .expect("the session did not stop while its socket write was blocked");

        assert!(result.unwrap_err().to_string().contains("forgotten"));
        assert!(button_released(injector.0.last()), "{:?}", injector.0);
    }

    #[tokio::test]
    async fn stops_when_trust_ends() {
        let (local, _peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (_capture, input) = mpsc::channel(16);
        let until = async { anyhow::anyhow!("the peer was forgotten") };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            until,
        );
        let error = tokio::time::timeout(Duration::from_secs(1), session)
            .await
            .expect("the session outlived trust")
            .unwrap_err();
        assert!(error.to_string().contains("forgotten"), "{error}");
    }

    #[tokio::test]
    async fn stops_when_trust_ends_while_socket_write_is_blocked() {
        let (local, _peer_that_never_reads) = channels_with_capacity(1).await;
        let control = Arc::new(SharedControl::new(true));
        let (_capture, input) = mpsc::channel(16);
        let until = async {
            tokio::time::sleep(Duration::from_millis(50)).await;
            anyhow::anyhow!("the peer was forgotten")
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            until,
        );
        let error = tokio::time::timeout(Duration::from_secs(1), session)
            .await
            .expect("a blocked writer prevented trust revocation")
            .unwrap_err();
        assert!(error.to_string().contains("forgotten"), "{error}");
    }

    #[tokio::test(start_paused = true)]
    async fn gives_up_on_a_silent_peer_while_socket_write_is_blocked() {
        let (local, _peer_that_never_reads) = channels_with_capacity(1).await;
        let control = Arc::new(SharedControl::new(true));
        let (_capture, input) = mpsc::channel(16);
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let mut board = no_clipboard();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let error = tokio::time::timeout(Duration::from_secs(5), session)
            .await
            .expect("a blocked writer prevented silence detection")
            .unwrap_err();
        assert!(error.to_string().contains("stopped responding"), "{error}");
    }

    fn board_with(content: Content, on: bool) -> Sharing<Board> {
        let board = Board::default();
        board.copy(content);
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
    async fn sends_its_clipboard_right_after_crossing() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("copied on this system"), true);
        let enter = Message::SharedEnter {
            generation: 0,
            along: 7,
        };
        capture.try_send(enter.clone()).unwrap();
        let script = async {
            let seen = until_snapshot_done(&mut peer).await;
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert_eq!(seen[0], enter);
        assert_eq!(assembled(&seen), Some(text("copied on this system")));
    }

    #[tokio::test]
    async fn writes_the_clipboard_that_comes_back() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(16);
        let mut board = no_clipboard();
        let script = async {
            // control comes back to this system, then the peer's clipboard follows
            peer.send(&Message::SharedLeave {
                generation: 0,
                along: 3,
            })
            .await
            .unwrap();
            let mut theirs = board_with(text("copied on the peer"), true);
            for part in theirs.crossing().map(|read| read()).unwrap_or_default() {
                peer.send(&Message::Clipboard { part }).await.unwrap();
            }
            round_trip(&mut peer, 1).await;
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (_, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert_eq!(board.clipboard().content(), Some(text("copied on the peer")));
    }

    /// The peer takes control of this system, something new is copied here
    /// while the peer is in control, then the peer hands control back with
    /// `hand_back`.
    /// Returns what this system sent from then on.
    async fn clipboard_after_handing_back(hand_back: &[Message]) -> Vec<Message> {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("copied before the peer took control"), true);
        let here = board.clipboard().clone();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            until_snapshot_done(&mut peer).await;
            peer.send(&Message::SharedEnter {
                generation: 1,
                along: 0,
            })
            .await
            .unwrap();
            round_trip(&mut peer, 1).await;
            here.copy(text("copied while the peer was in control"));
            for message in hand_back {
                peer.send(message).await.unwrap();
            }
            let seen = until_snapshot_done(&mut peer).await;
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();
        result.unwrap();
        seen
    }

    #[tokio::test]
    async fn sends_its_clipboard_when_control_leaves() {
        let seen = clipboard_after_handing_back(&[Message::SharedInput {
            generation: 1,
            event: AWAY,
        }])
        .await;
        assert_eq!(
            seen[0],
            Message::SharedLeave {
                generation: 1,
                along: 0
            }
        );
        assert_eq!(assembled(&seen), Some(text("copied while the peer was in control")));
    }

    #[tokio::test]
    async fn sends_its_clipboard_when_control_is_reclaimed() {
        let seen = clipboard_after_handing_back(&[Message::SharedReclaim { generation: 1 }]).await;
        assert_eq!(assembled(&seen), Some(text("copied while the peer was in control")));
    }

    #[tokio::test]
    async fn switched_off_sends_and_writes_nothing() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(false));
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("stays on this system"), false);
        let script = async {
            peer_enters(&mut peer).await;
            let mut theirs = board_with(text("from the peer"), true);
            for part in theirs.crossing().map(|read| read()).unwrap_or_default() {
                peer.send(&Message::Clipboard { part }).await.unwrap();
            }
            peer_sends(&mut peer, AWAY).await;
            let mut seen = round_trip(&mut peer, 1).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
            seen.extend(round_trip(&mut peer, 2).await);
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen.retain(|m| {
                !matches!(
                    m,
                    Message::Clipboard {
                        part: ClipboardPart::Ack
                    }
                )
            });
            seen
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert_eq!(
            seen,
            vec![Message::SharedLeave {
                generation: 1,
                along: 0
            }],
            "this system sent its clipboard while sharing was off"
        );
        assert_eq!(board.clipboard().content(), Some(text("stays on this system")));
    }

    #[tokio::test]
    async fn a_large_image_neither_overloads_the_session_nor_holds_input_back() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(16);
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 70]),
            ..Content::default()
        };
        let mut board = board_with(image.clone(), true);
        capture
            .try_send(Message::SharedEnter {
                generation: 0,
                along: 7,
            })
            .unwrap();
        let script = async {
            let mut seen = Vec::new();
            let mut sent_motion = false;
            loop {
                let message = peer.recv().await.unwrap();
                let chunk = matches!(
                    message,
                    Message::Clipboard {
                        part: ClipboardPart::Chunk { .. }
                    }
                );
                if chunk {
                    peer.send(&Message::Clipboard {
                        part: ClipboardPart::Ack,
                    })
                    .await
                    .unwrap();
                    if !sent_motion {
                        capture
                            .try_send(Message::SharedInput {
                                generation: 0,
                                event: InputEvent::Motion { dx: 1.0, dy: 0.0 },
                            })
                            .unwrap();
                        sent_motion = true;
                    }
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
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            seen
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (seen, result) = tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(script, session) })
            .await
            .unwrap();

        result.unwrap();
        assert_eq!(assembled(&seen), Some(image));
        let motion = seen
            .iter()
            .position(|m| matches!(m, Message::SharedInput { .. }))
            .unwrap();
        assert!(motion < seen.len() - 2, "input waited for the whole image");
    }

    #[tokio::test]
    async fn a_peer_that_does_not_acknowledge_gets_no_more_than_the_window() {
        let (local, mut peer) = channels().await;
        let control = Arc::new(SharedControl::new(true));
        let (capture, input) = mpsc::channel(16);
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 50]),
            ..Content::default()
        };
        let mut board = board_with(image, true);
        capture
            .try_send(Message::SharedEnter {
                generation: 0,
                along: 7,
            })
            .unwrap();
        let script = async {
            let mut chunks = 0;
            // read everything this system sends for a while, acknowledging nothing
            let _ = tokio::time::timeout(Duration::from_millis(400), async {
                loop {
                    if let Message::Clipboard {
                        part: ClipboardPart::Chunk { .. },
                    } = peer.recv().await.unwrap()
                    {
                        chunks += 1;
                    }
                }
            })
            .await;
            drop(capture);
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(peer);
            chunks
        };
        let mut pointer = Returned::default();
        let mut injector = Recorded::default();
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (chunks, _) = tokio::time::timeout(Duration::from_secs(2), async { tokio::join!(script, session) })
            .await
            .unwrap();
        assert_eq!(chunks, crate::clipboard::WINDOW);
    }

    #[test]
    fn a_close_after_silence_counts_as_a_lost_connection() {
        assert!(closed_after(Duration::from_millis(200)).is_ok());
        let lost = closed_after(SILENCE_LIMIT + Duration::from_millis(1)).unwrap_err();
        assert!(connection_lost(&lost));
    }
}
