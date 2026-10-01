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
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::clipboard::{Clipboard, Sharing, WINDOW};
use crate::identity::PublicKey;
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
    /// The peer now sits at `side`.
    fn arrange(&mut self, _side: Side) {}
}

pub struct SharedLayout {
    pub screen: Rect,
    pub side: Side,
    pub control: std::sync::Arc<crate::control::SharedControl>,
    pub arranging: Arranging,
}

/// Rearranging the two screens while the session runs.
pub struct Arranging {
    /// The relation both systems agreed on, and when it was chosen.
    pub agreed: (Side, crate::trust::Timestamp),
    pub initiator: bool,
    /// Sides chosen on this system during the session.
    pub choices: tokio::sync::watch::Receiver<Option<Side>>,
    /// Each newly agreed relation, to store and show.
    pub agreed_tx: mpsc::UnboundedSender<(Side, crate::trust::Timestamp)>,
}

impl Arranging {
    /// Agreed on `agreed`, with no way to choose a side here.
    pub fn fixed(agreed: (Side, crate::trust::Timestamp), initiator: bool) -> Self {
        Self {
            agreed,
            initiator,
            choices: tokio::sync::watch::channel(None).1,
            agreed_tx: mpsc::unbounded_channel().0,
        }
    }
}

/// What one system's group shares across all its links: one event tap, one
/// owner of control and one replay of whoever drives this system.
pub struct Group {
    pub screen: Rect,
    pub side: Side,
    pub control: std::sync::Arc<crate::control::SharedControl>,
    /// Sides chosen on this system, applied to every link.
    pub choices: tokio::sync::watch::Receiver<Option<Side>>,
}

/// A session with one more peer, joining the group.
pub struct Joining<S> {
    pub channel: Channel<S>,
    /// The pairwise relation agreed with this peer, and how to store changes.
    pub agreed: (Side, crate::trust::Timestamp),
    pub initiator: bool,
    pub agreed_tx: mpsc::UnboundedSender<(Side, crate::trust::Timestamp)>,
    /// How the link ended: `Ok` when the peer closed it.
    pub done: oneshot::Sender<Result<()>>,
}

pub enum Membership<S> {
    Join(Joining<S>),
    /// Drop the link with this peer, as when its trust ends.
    Drop(PublicKey),
}

/// Both sides capture physical input and either may take control. Generation
/// stamps exclude queued events from the previous owner after a handoff.
pub async fn together<S>(
    channel: Channel<S>,
    layout: SharedLayout,
    input: mpsc::Receiver<Message>,
    pointer: &mut impl Pointer,
    injector: &mut impl Inject,
    sharing: &mut Sharing<impl Clipboard>,
    until: impl Future<Output = anyhow::Error>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (members, membership) = mpsc::unbounded_channel();
    let (done, ended) = oneshot::channel();
    let _ = members.send(Membership::Join(Joining {
        channel,
        agreed: layout.arranging.agreed,
        initiator: layout.arranging.initiator,
        agreed_tx: layout.arranging.agreed_tx,
        done,
    }));
    let group = Group {
        screen: layout.screen,
        side: layout.side,
        control: layout.control,
        choices: layout.arranging.choices,
    };
    let mut membership = membership;
    let result = run(
        group,
        VecDeque::new(),
        &mut membership,
        input,
        pointer,
        injector,
        sharing,
        until,
    )
    .await;
    // the link's own ending says more than the group's
    let mut ended = ended;
    ended.try_recv().unwrap_or(result)
}

/// One peer's session within the group.
struct Link {
    outgoing: Outgoing,
    _incoming: Incoming,
    meter: crate::latency::Meter,
    last_heard: Instant,
    nonce: u64,
    agreed: (Side, crate::trust::Timestamp),
    initiator: bool,
    agreed_tx: mpsc::UnboundedSender<(Side, crate::trust::Timestamp)>,
    done: Option<oneshot::Sender<Result<()>>>,
}

impl Link {
    fn finish(mut self, result: Result<()>) {
        if let Some(done) = self.done.take() {
            let _ = done.send(result);
        }
    }
}

type Received = (PublicKey, Result<Message, SessionError>);

/// Runs this system's side of a group: links join and leave through
/// `waiting` and then `membership`, while one capture, one owner of control
/// and one replay are shared by all of them. Returns once the last link has
/// ended, or when `until` fires or capture stops.
#[allow(clippy::too_many_arguments)]
pub async fn run<S>(
    group: Group,
    waiting: VecDeque<Membership<S>>,
    membership: &mut mpsc::UnboundedReceiver<Membership<S>>,
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
    let control = group.control;
    let mut choices = group.choices;
    let mut target = Target::new(group.screen, group.side.opposite());
    let release = ReleaseOnDrop {
        target: &mut target,
        injector,
    };
    let mut wakeups = control
        .take_wakeups()
        .context("the input control is already in use by another session")?;
    let (received_tx, mut received) = mpsc::channel::<Received>(INCOMING_CAPACITY);
    let mut links: std::collections::BTreeMap<PublicKey, Link> = std::collections::BTreeMap::new();
    // the peer the pointer crossed onto, while this system drives it
    let mut crossed: Option<PublicKey> = None;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut ended: Vec<(PublicKey, Result<()>)> = Vec::new();
    // links already waiting join before any input is routed
    let mut waiting = waiting;
    waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
    loop {
        for (key, result) in ended.drain(..) {
            let Some(link) = links.remove(&key) else { continue };
            link.finish(result);
            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
            let was_owner = state.owner() == key;
            let generation = was_owner.then(|| state.interrupt(control.now()));
            drop(state);
            if was_owner {
                for action in release.target.reclaim() {
                    release.injector.execute(&action);
                }
            }
            if crossed == Some(key) {
                crossed = None;
                pointer.yield_control();
            }
            if let Some(generation) = generation {
                for link in links.values() {
                    let _ = link.outgoing.send(Message::ControlClaim { generation });
                }
            }
            publish(&control, &links);
        }
        while let Some(change) = waiting.pop_front() {
            match change {
                Membership::Join(joining) => {
                    let key = joining.channel.remote_key();
                    let (sender, receiver) = joining.channel.split();
                    let link = Link {
                        outgoing: spawn_sender(sender),
                        _incoming: spawn_receiver(key, receiver, received_tx.clone()),
                        meter: crate::latency::Meter::default(),
                        last_heard: Instant::now(),
                        nonce: 0,
                        agreed: joining.agreed,
                        initiator: joining.initiator,
                        agreed_tx: joining.agreed_tx,
                        done: Some(joining.done),
                    };
                    let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                    let current = Message::ControlState {
                        generation: state.generation(),
                        owner: state.owner(),
                    };
                    drop(state);
                    if let Err(error) = link.outgoing.send(current) {
                        link.finish(Err(error));
                    } else if let Some(replaced) = links.insert(key, link) {
                        replaced.finish(Err(anyhow::anyhow!("the peer connected again")));
                    }
                    publish(&control, &links);
                }
                Membership::Drop(key) => ended.push((key, Err(anyhow::anyhow!("the link was dropped")))),
            }
        }
        if !ended.is_empty() {
            continue;
        }
        if links.is_empty() {
            waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
            if waiting.is_empty() {
                return Ok(());
            }
            continue;
        }
        tokio::select! {
            change = membership.recv() => match change {
                Some(change) => waiting.push_back(change),
                None => std::future::pending::<()>().await,
            },
            _ = wakeups.recv() => {
                let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                let interrupted = control.interrupted.swap(false, std::sync::atomic::Ordering::AcqRel);
                let claim = if interrupted { Some(state.interrupt(control.now())) } else { None };
                drop(state);
                for action in release.target.reclaim() { release.injector.execute(&action); }
                if interrupted {
                    pointer.yield_control();
                    crossed = None;
                }
                if let Some(generation) = claim {
                    for (key, link) in &links {
                        if let Err(error) = link.outgoing.send(Message::ControlClaim { generation }) {
                            ended.push((*key, Err(error)));
                        }
                    }
                    sharing.expect_snapshot();
                }
                publish(&control, &links);
            }
            message = input.recv() => {
                let Some(message) = message else {
                    let mut result = Ok(());
                    for (_, link) in std::mem::take(&mut links) {
                        let drained = link.outgoing.drain().await;
                        if result.is_ok() { result = drained; }
                    }
                    return result;
                };
                match message {
                    Message::ControlClaim { .. } => {
                        for action in release.target.reclaim() { release.injector.execute(&action); }
                        sharing.expect_snapshot();
                        for (key, link) in &links {
                            if let Err(error) = link.outgoing.send(message.clone()) {
                                ended.push((*key, Err(error)));
                            }
                        }
                    }
                    Message::Enter { along, .. } => {
                        // until routing follows the group's layout, the pointer
                        // crosses to the peer on the chosen side, or any peer
                        let side = choices.borrow().unwrap_or(group.side);
                        let destination = links
                            .iter()
                            .find(|(_, link)| link.agreed.0 == side)
                            .or_else(|| links.iter().next())
                            .map(|(key, _)| *key);
                        match destination.and_then(|key| links.get(&key).map(|link| (key, link))) {
                            Some((key, link)) => {
                                crossed = Some(key);
                                if let Err(error) = link.outgoing.send(message) {
                                    ended.push((key, Err(error)));
                                } else {
                                    link.outgoing.send_clipboard(sharing.crossing());
                                }
                            }
                            None => pointer.leave(along),
                        }
                    }
                    other => {
                        let reclaiming = matches!(other, Message::Reclaim { .. });
                        if let Some(key) = crossed
                            && let Some(link) = links.get(&key)
                            && let Err(error) = link.outgoing.send(other)
                        {
                            ended.push((key, Err(error)));
                        }
                        if reclaiming { crossed = None; }
                    }
                }
            }
            event = received.recv() => {
                let Some((peer, message)) = event else { continue };
                let Some(link) = links.get_mut(&peer) else { continue };
                let quiet = link.last_heard.elapsed();
                link.last_heard = Instant::now();
                let message = match message {
                    Ok(message) => message,
                    Err(SessionError::Closed) => { ended.push((peer, closed_after(quiet))); continue; }
                    Err(error) => { ended.push((peer, Err(error.into()))); continue; }
                };
                let outcome: Result<()> = (|| {
                    match message {
                        Message::Ping { nonce } => link.outgoing.send(Message::Pong { nonce })?,
                        Message::Pong { nonce } => {
                            if let Some(round_trip) = link.meter.answered(nonce, control.now())
                                && round_trip > crate::latency::SPIKE
                            {
                                tracing::warn!(ms = round_trip.as_millis(), "slow round trip to the peer");
                            }
                        }
                        Message::Clipboard { part } => receive_clipboard(part, &link.outgoing, sharing)?,
                        Message::Layout { side, chosen } => {
                            let agreed = crate::control::agreed_side(link.initiator, link.agreed, (side, chosen));
                            if agreed != link.agreed {
                                link.agreed = agreed;
                                release.target.arrange(agreed.0);
                                pointer.arrange(agreed.0);
                                let _ = link.agreed_tx.send(agreed);
                            }
                        }
                        Message::ControlState { generation, owner } => {
                            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let was_mine = state.owns();
                            let changed = state.claim(generation, owner);
                            let mine = state.owns();
                            drop(state);
                            if changed && was_mine && !mine {
                                pointer.yield_control();
                                crossed = None;
                                for action in release.target.reclaim() { release.injector.execute(&action); }
                            }
                        }
                        Message::ControlClaim { generation } => {
                            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let changed = state.claim(generation, peer);
                            drop(state);
                            if changed {
                                pointer.yield_control();
                                crossed = None;
                                for action in release.target.reclaim() { release.injector.execute(&action); }
                                link.outgoing.send_clipboard(sharing.crossing());
                            }
                        }
                        Message::Leave { generation, along } => {
                            let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let current = state.owns() && state.generation() == generation;
                            drop(state);
                            if current && crossed == Some(peer) {
                                crossed = None;
                                pointer.leave(along);
                                sharing.expect_snapshot();
                            }
                        }
                        Message::Enter { generation, .. }
                        | Message::Input { generation, .. }
                        | Message::Reclaim { generation } => {
                            let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let receives = state.receives(generation, peer, control.now()) && !control.local_busy();
                            drop(state);
                            if !receives {
                                if let Message::Enter { along, .. } = message {
                                    link.outgoing.send(Message::Leave { generation, along })?;
                                }
                                return Ok(());
                            }
                            let mut crossing = false;
                            let actions = match message {
                                Message::Enter { along, .. } => { sharing.expect_snapshot(); release.target.enter(along) },
                                Message::Input { event, .. } => release.target.input(event),
                                Message::Reclaim { .. } => { crossing = true; release.target.reclaim() },
                                _ => unreachable!(),
                            };
                            for action in actions {
                                match action {
                                    Action::Leave { along } => { link.outgoing.send(Message::Leave { generation, along })?; crossing = true; },
                                    other => release.injector.execute(&other),
                                }
                            }
                            if crossing { link.outgoing.send_clipboard(sharing.crossing()); }
                        }
                        other => bail!("unexpected message in shared session: {other:?}"),
                    }
                    Ok(())
                })();
                if let Err(error) = outcome { ended.push((peer, Err(error))); }
                publish(&control, &links);
            }
            Ok(()) = choices.changed() => {
                let chosen = *choices.borrow_and_update();
                if let Some(side) = chosen {
                    let mut moved = false;
                    for (key, link) in links.iter_mut() {
                        if side == link.agreed.0 { continue; }
                        moved = true;
                        link.agreed = (side, crate::trust::now());
                        if let Err(error) = link.outgoing.send(Message::Layout { side, chosen: link.agreed.1 }) {
                            ended.push((*key, Err(error)));
                        }
                        let _ = link.agreed_tx.send(link.agreed);
                    }
                    if moved {
                        release.target.arrange(side);
                        pointer.arrange(side);
                    }
                }
            }
            error = &mut until => return Err(error),
            _ = heartbeat.tick() => {
                for (key, link) in links.iter_mut() {
                    if link.last_heard.elapsed() > SILENCE_LIMIT {
                        ended.push((*key, Err(Silent.into())));
                        continue;
                    }
                    if link.outgoing.stopped() {
                        ended.push((*key, link.outgoing.finished().await));
                        continue;
                    }
                    link.nonce += 1;
                    link.meter.sent(link.nonce, control.now());
                    if let Err(error) = link.outgoing.send(Message::Ping { nonce: link.nonce }) {
                        ended.push((*key, Err(error)));
                    }
                }
                publish(&control, &links);
            }
        }
    }
}

/// Reports who has control, and the round trip to the peer this system
/// last heard from with a measure.
fn publish(control: &crate::control::SharedControl, links: &std::collections::BTreeMap<PublicKey, Link>) {
    control.publish(links.values().find_map(|link| link.meter.average()));
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
    task: JoinHandle<()>,
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

    fn stopped(&self) -> bool {
        self.task.is_finished()
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
fn spawn_receiver<R>(key: PublicKey, mut receiver: ChannelReceiver<R>, sender: mpsc::Sender<Received>) -> Incoming
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let task = tokio::spawn(async move {
        loop {
            let message = receiver.recv().await;
            let failed = message.is_err();
            if sender.send((key, message)).await.is_err() || failed {
                break;
            }
        }
    });
    Incoming { task }
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

    /// This system's control for a session on `local`; `owns` says whether
    /// it starts with control and wins a tie with the peer.
    fn control(local: &Channel<DuplexStream>, owns: bool) -> Arc<SharedControl> {
        let peer = local.remote_key();
        let me = crate::identity::PublicKey::from_bytes(&[if owns { 0xff } else { 0 }; 32]).unwrap();
        Arc::new(SharedControl::new(me, if owns { me } else { peer }))
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
        let key = channel.remote_key();
        let (_sender, receiver) = channel.split();
        let (received, _unread) = mpsc::channel(1);
        let incoming = spawn_receiver(key, receiver, received);
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
        let control = control(&local, true);
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
                arranging: Arranging::fixed((Side::Left, 0), true),
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
        let control = control(&local, true);
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
                arranging: Arranging::fixed((Side::Left, 0), true),
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
        let control = control(&local, false);
        let (_capture, input) = mpsc::channel(16);
        let mut injector = Recorded::default();
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer.send(&Message::Ping { nonce: 1 }).await.unwrap();
            while peer.recv().await.unwrap() != (Message::Pong { nonce: 1 }) {}
            control.note_physical();
            peer.send(&Message::Enter {
                generation: 1,
                along: 123,
            })
            .await
            .unwrap();
            loop {
                match peer.recv().await.unwrap() {
                    Message::Leave { generation, along } => break (generation, along),
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
                arranging: Arranging::fixed((Side::Left, 0), true),
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
        let control = control(&local, false);
        let (capture, input) = mpsc::channel(16);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let mut injector = SharedRecorded(actions.clone());
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer.send(&Message::Enter {
                generation: 1,
                along: 0,
            })
            .await
            .unwrap();
            peer.send(&Message::Input {
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
            peer.send(&Message::Enter {
                generation: 3,
                along: 0,
            })
            .await
            .unwrap();
            peer.send(&Message::Input {
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
            peer.send(&Message::Input {
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
                arranging: Arranging::fixed((Side::Left, 0), true),
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

    #[derive(Default)]
    struct Arranged(Vec<Side>);

    impl Pointer for Arranged {
        fn leave(&mut self, _along: Along) {}
        fn arrange(&mut self, side: Side) {
            self.0.push(side);
        }
    }

    /// Runs a session that agreed the peer sits on the left, chosen at time
    /// 100, against `script`, which plays the peer.
    async fn arranging<F: Future>(
        initiator: bool,
        script: impl FnOnce(Channel<DuplexStream>, tokio::sync::watch::Sender<Option<Side>>) -> F,
    ) -> (F::Output, Vec<(Side, crate::trust::Timestamp)>, Vec<Side>) {
        let (local, peer) = channels().await;
        let control = control(&local, initiator);
        let (_capture, input) = mpsc::channel(16);
        let (choose, choices) = tokio::sync::watch::channel(None);
        let (agreed_tx, mut agreed_rx) = mpsc::unbounded_channel();
        let mut injector = Recorded::default();
        let mut pointer = Arranged::default();
        let mut board = no_clipboard();
        let layout = SharedLayout {
            arranging: Arranging {
                agreed: (Side::Left, 100),
                initiator,
                choices,
                agreed_tx,
            },
            ..layout(&control)
        };
        let session = together(local, layout, input, &mut pointer, &mut injector, &mut board, pending());
        let played = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                played = script(peer, choose) => played,
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        let mut agreed = Vec::new();
        while let Ok(next) = agreed_rx.try_recv() {
            agreed.push(next);
        }
        (played, agreed, pointer.0)
    }

    /// Waits until the session has handled everything sent before.
    async fn settle(peer: &mut Channel<DuplexStream>, nonce: u64) {
        peer.send(&Message::Ping { nonce }).await.unwrap();
        while peer.recv().await.unwrap() != (Message::Pong { nonce }) {}
    }

    #[tokio::test]
    async fn a_later_arrangement_from_the_peer_moves_this_screen() {
        let ((), agreed, arranged) = arranging(true, |mut peer, _choose| async move {
            // the peer now places this system on its left, so the peer sits on the right
            peer.send(&Message::Layout {
                side: Side::Left,
                chosen: 200,
            })
            .await
            .unwrap();
            settle(&mut peer, 1).await;
            // an older choice does not undo it
            peer.send(&Message::Layout {
                side: Side::Below,
                chosen: 150,
            })
            .await
            .unwrap();
            settle(&mut peer, 2).await;
        })
        .await;
        assert_eq!(agreed, [(Side::Right, 200)]);
        assert_eq!(arranged, [Side::Right]);
    }

    #[tokio::test]
    async fn a_side_chosen_here_is_sent_to_the_peer() {
        let (sent, agreed, arranged) = arranging(false, |mut peer, choose| async move {
            choose.send_replace(Some(Side::Above));
            loop {
                if let Message::Layout { side, chosen } = peer.recv().await.unwrap() {
                    break (side, chosen);
                }
            }
        })
        .await;
        assert_eq!(sent.0, Side::Above);
        assert!(sent.1 > 100);
        assert_eq!(agreed, [sent]);
        assert_eq!(arranged, [Side::Above]);
    }

    #[tokio::test]
    async fn a_session_that_cannot_choose_still_follows_the_peer() {
        let ((), agreed, arranged) = arranging(true, |mut peer, choose| async move {
            drop(choose);
            settle(&mut peer, 1).await;
            peer.send(&Message::Layout {
                side: Side::Left,
                chosen: 200,
            })
            .await
            .unwrap();
            settle(&mut peer, 2).await;
        })
        .await;
        assert_eq!(agreed, [(Side::Right, 200)]);
        assert_eq!(arranged, [Side::Right]);
    }

    #[tokio::test]
    async fn choosing_the_current_side_changes_nothing() {
        let ((), agreed, arranged) = arranging(false, |mut peer, choose| async move {
            choose.send_replace(Some(Side::Left));
            settle(&mut peer, 1).await;
        })
        .await;
        assert!(agreed.is_empty());
        assert!(arranged.is_empty());
    }

    /// A group on this system with one link per channel. Returns this
    /// system's key, the far ends of the links, and how each link ends.
    async fn group_of(
        count: usize,
        owner: Option<usize>,
    ) -> (
        Arc<SharedControl>,
        mpsc::UnboundedSender<Membership<DuplexStream>>,
        mpsc::UnboundedReceiver<Membership<DuplexStream>>,
        Vec<Channel<DuplexStream>>,
        Vec<oneshot::Receiver<Result<()>>>,
    ) {
        let me = Identity::generate().unwrap();
        let (members, membership) = mpsc::unbounded_channel();
        let (mut far, mut ended) = (Vec::new(), Vec::new());
        let mut keys = Vec::new();
        for _ in 0..count {
            let (a, b) = duplex(1 << 17);
            let them = Identity::generate().unwrap();
            let (here, there) = tokio::join!(Channel::initiate(a, &me), Channel::respond(b, &them));
            let (here, there) = (here.unwrap(), there.unwrap());
            keys.push(here.remote_key());
            let (done, end) = oneshot::channel();
            members
                .send(Membership::Join(Joining {
                    channel: here,
                    agreed: (Side::Left, 0),
                    initiator: true,
                    agreed_tx: mpsc::unbounded_channel().0,
                    done,
                }))
                .ok()
                .unwrap();
            far.push(there);
            ended.push(end);
        }
        let mine = me.public_key();
        let control = Arc::new(SharedControl::new(mine, owner.map_or(mine, |index| keys[index])));
        (control, members, membership, far, ended)
    }

    fn group(control: &Arc<SharedControl>) -> Group {
        Group {
            screen: SCREEN,
            side: Side::Left,
            control: control.clone(),
            choices: watch::channel(None).1,
        }
    }

    /// The next message from this system other than heartbeats.
    async fn next(peer: &mut Channel<DuplexStream>) -> Message {
        loop {
            match peer.recv().await.unwrap() {
                Message::Ping { nonce } => peer.send(&Message::Pong { nonce }).await.unwrap(),
                other => return other,
            }
        }
    }

    #[tokio::test]
    async fn every_member_learns_who_has_control_when_it_joins() {
        let (control, _members, mut membership, mut far, _ended) = group_of(2, Some(0)).await;
        let expected = control.state.lock().unwrap().owner();
        let (_capture, input) = mpsc::channel(16);
        let (mut pointer, mut injector, mut board) = (Returned::default(), Recorded::default(), no_clipboard());
        let run = run(
            group(&control),
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            for peer in far.iter_mut() {
                assert_eq!(
                    next(peer).await,
                    Message::ControlState {
                        generation: 0,
                        owner: expected
                    }
                );
            }
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_claim_reaches_every_member_and_only_the_owner_is_played() {
        let (control, _members, mut membership, mut far, _ended) = group_of(2, None).await;
        let (capture, input) = mpsc::channel(16);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let mut injector = SharedRecorded(actions.clone());
        let (mut pointer, mut board) = (Returned::default(), no_clipboard());
        let run = run(
            group(&control),
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            for peer in far.iter_mut() {
                assert!(matches!(next(peer).await, Message::ControlState { .. }));
            }
            // the first member takes control and crosses onto this system
            far[0].send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer_enters(&mut far[0]).await;
            // the second member, without control, tries the same
            far[1]
                .send(&Message::Enter {
                    generation: 1,
                    along: 0,
                })
                .await
                .unwrap();
            assert_eq!(
                next(&mut far[1]).await,
                Message::Leave {
                    generation: 1,
                    along: 0
                }
            );
            far[0]
                .send(&Message::Input {
                    generation: 1,
                    event: PRESS_KEY,
                })
                .await
                .unwrap();
            round_trip(&mut far[0], 1).await;
            assert!(
                actions
                    .lock()
                    .unwrap()
                    .iter()
                    .any(|a| matches!(a, Action::Key { down: true, .. }))
            );
            // this system takes control back: both members hear it
            let generation = control.state.lock().unwrap().physical(control.now()).unwrap();
            capture.send(Message::ControlClaim { generation }).await.unwrap();
            for peer in far.iter_mut() {
                loop {
                    if next(peer).await == (Message::ControlClaim { generation }) {
                        break;
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert!(
            actions
                .lock()
                .unwrap()
                .iter()
                .any(|a| matches!(a, Action::Key { down: false, .. }))
        );
    }

    #[tokio::test]
    async fn losing_the_member_in_control_takes_control_back_and_keeps_the_rest() {
        let (control, _members, mut membership, mut far, mut ended) = group_of(2, None).await;
        let (_capture, input) = mpsc::channel(16);
        let actions = Arc::new(Mutex::new(Vec::new()));
        let mut injector = SharedRecorded(actions.clone());
        let (mut pointer, mut board) = (Returned::default(), no_clipboard());
        let run = run(
            group(&control),
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            let mut owner = far.remove(0);
            let mut other = far.remove(0);
            next(&mut owner).await;
            next(&mut other).await;
            owner.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            peer_enters(&mut owner).await;
            peer_sends(&mut owner, PRESS_KEY).await;
            round_trip(&mut owner, 1).await;
            drop(owner);
            let first = ended.remove(0);
            assert!(first.await.unwrap().is_ok(), "a closed link ends cleanly");
            // the remaining member hears this system take control back
            let claim = next(&mut other).await;
            assert!(matches!(claim, Message::ControlClaim { generation: 2 }), "{claim:?}");
            round_trip(&mut other, 2).await;
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert!(control.state.lock().unwrap().owns());
        assert!(
            actions
                .lock()
                .unwrap()
                .iter()
                .any(|a| matches!(a, Action::Key { down: false, .. }))
        );
    }

    fn layout(control: &Arc<SharedControl>) -> SharedLayout {
        SharedLayout {
            screen: SCREEN,
            side: Side::Left,
            control: control.clone(),
            arranging: Arranging::fixed((Side::Left, 0), true),
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
        peer.send(&Message::Enter {
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
        peer.send(&Message::Input { generation: 1, event }).await.unwrap();
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
                Message::Ping { .. } | Message::ControlState { .. } => {}
                other => seen.push(other),
            }
        }
    }

    #[tokio::test]
    async fn replays_input_and_hands_control_back() {
        let (local, mut peer) = channels().await;
        let control = control(&local, false);
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
            seen.contains(&Message::Leave {
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
        let control = control(&local, false);
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
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        capture
            .try_send(Message::Enter {
                generation: 0,
                along: 7,
            })
            .unwrap();
        let script = async {
            let mut seen = Vec::new();
            while seen.is_empty() {
                match peer.recv().await.unwrap() {
                    Message::Ping { .. } | Message::ControlState { .. } => {}
                    other => seen.push(other),
                }
            }
            peer.send(&Message::Leave {
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
            vec![Message::Enter {
                generation: 0,
                along: 7
            }]
        );
        assert_eq!(pointer.0, vec![9]);
    }

    #[tokio::test]
    async fn flushes_accepted_input_when_its_queue_closes() {
        let (local, mut peer) = channels_with_capacity(1).await;
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(2);
        let enter = Message::Enter {
            generation: 0,
            along: 7,
        };
        capture.try_send(enter.clone()).unwrap();
        drop(capture);
        let receive = async {
            loop {
                match peer.recv().await.unwrap() {
                    Message::Ping { .. } | Message::ControlState { .. } => {}
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
        let control = control(&local, true);
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
        let control = control(&local, false);
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
        let control = control(&local, false);
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
        let control = control(&local, false);
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
        let control = control(&local, true);
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
        let control = control(&local, true);
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
        let control = control(&local, true);
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
                Message::Ping { .. } | Message::ControlState { .. } => {}
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
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("copied on this system"), true);
        let enter = Message::Enter {
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
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        capture
            .try_send(Message::Enter {
                generation: 0,
                along: 3,
            })
            .unwrap();
        let mut board = no_clipboard();
        let script = async {
            // the pointer crossed; control comes back, then the peer's clipboard follows
            while !matches!(peer.recv().await.unwrap(), Message::Enter { .. }) {}
            peer.send(&Message::Leave {
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
        let control = control(&local, false);
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("copied before the peer took control"), true);
        let here = board.clipboard().clone();
        let script = async {
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            until_snapshot_done(&mut peer).await;
            peer.send(&Message::Enter {
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
        let seen = clipboard_after_handing_back(&[Message::Input {
            generation: 1,
            event: AWAY,
        }])
        .await;
        assert_eq!(
            seen[0],
            Message::Leave {
                generation: 1,
                along: 0
            }
        );
        assert_eq!(assembled(&seen), Some(text("copied while the peer was in control")));
    }

    #[tokio::test]
    async fn sends_its_clipboard_when_control_is_reclaimed() {
        let seen = clipboard_after_handing_back(&[Message::Reclaim { generation: 1 }]).await;
        assert_eq!(assembled(&seen), Some(text("copied while the peer was in control")));
    }

    #[tokio::test]
    async fn switched_off_sends_and_writes_nothing() {
        let (local, mut peer) = channels().await;
        let control = control(&local, false);
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
            vec![Message::Leave {
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
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 70]),
            ..Content::default()
        };
        let mut board = board_with(image.clone(), true);
        capture
            .try_send(Message::Enter {
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
                            .try_send(Message::Input {
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
        let motion = seen.iter().position(|m| matches!(m, Message::Input { .. })).unwrap();
        assert!(motion < seen.len() - 2, "input waited for the whole image");
    }

    #[tokio::test]
    async fn a_peer_that_does_not_acknowledge_gets_no_more_than_the_window() {
        let (local, mut peer) = channels().await;
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        let image = Content {
            png: Some(vec![9; crate::clipboard::CHUNK_LEN * 50]),
            ..Content::default()
        };
        let mut board = board_with(image, true);
        capture
            .try_send(Message::Enter {
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
