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
use crate::input::{Action, Point, Rect, Side, Target};
use crate::protocol::{ClipboardPart, Message};
use crate::session::{Channel, ChannelReceiver, ChannelSender, SessionError};

const HEARTBEAT: Duration = Duration::from_secs(1);
/// Silence after which the peer is presumed gone.
const SILENCE_LIMIT: Duration = Duration::from_secs(3);

/// Puts the pointer back on this system when control returns to it.
pub trait Pointer {
    /// Control came back with the pointer `at` a point on this system, or
    /// where it left when `None`.
    fn leave(&mut self, at: Option<Point>);
    fn yield_control(&mut self) {}
    /// The group was rearranged.
    fn arrange(&mut self, _layout: Layout) {}
}

pub type Layout = crate::layout::Group<PublicKey>;

/// A member's displays, and where a person dropped them in the group.
pub type Placing = (PublicKey, crate::layout::Offset);

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
    /// Members a person moved on this system during the session.
    pub choices: tokio::sync::watch::Receiver<Option<Placing>>,
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
    pub files: Option<std::sync::Arc<crate::file_transfer::Hub>>,
    /// This system's displays, in its own coordinates, as they change.
    pub displays: tokio::sync::watch::Receiver<Vec<Rect>>,
    pub control: std::sync::Arc<crate::control::SharedControl>,
    /// Members a person moved on this system: whose displays, and the offset
    /// they were dropped at.
    pub choices: tokio::sync::watch::Receiver<Option<Placing>>,
    /// The arrangement as it stands, for the window to draw.
    pub arranged: std::sync::Arc<tokio::sync::watch::Sender<Layout>>,
    /// The arrangement kept from before, and where to keep each new one.
    pub saved: Option<crate::peers::Arrangement>,
    pub save: mpsc::UnboundedSender<crate::peers::Arrangement>,
    /// Each link as a person sees it.
    pub reports: std::sync::Arc<tokio::sync::watch::Sender<Reports>>,
    /// Whether this system's screen is locked, as it changes.
    pub locked: tokio::sync::watch::Receiver<bool>,
    /// Introductions and revocations from members, with who sent each.
    pub trust: mpsc::UnboundedSender<(PublicKey, Message)>,
}

/// Each peer's link, as a person sees it.
pub type Reports = std::collections::BTreeMap<PublicKey, crate::control::Link>;

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
    /// Send this to every member.
    Send(Message),
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
        files: None,
        displays: tokio::sync::watch::channel(vec![layout.screen]).1,
        control: layout.control,
        choices: layout.arranging.choices,
        arranged: std::sync::Arc::new(tokio::sync::watch::Sender::new(Layout { members: Vec::new() })),
        saved: None,
        save: mpsc::unbounded_channel().0,
        reports: std::sync::Arc::new(tokio::sync::watch::Sender::new(Reports::new())),
        locked: tokio::sync::watch::channel(false).1,
        trust: mpsc::unbounded_channel().0,
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
    protocol: u16,
    files_capable: bool,
    /// Clipboard copies go by offer and request on this link.
    clipboard_ids: bool,
    trace_capable: bool,
    trace_sent: bool,
    trace_request: crate::diagnostics::Lease,
    outgoing: Outgoing,
    _incoming: Incoming,
    meter: crate::latency::Meter,
    last_heard: Instant,
    nonce: u64,
    locked: bool,
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
    let files = group.files;
    let mut file_messages = files.as_ref().map(|files| FileMessages {
        hub: files.clone(),
        receiver: Some(files.take_messages()),
    });
    let mut choices = group.choices;
    let mut displays = group.displays;
    let mut locked = group.locked;
    let me = control.state.lock().unwrap_or_else(|e| e.into_inner()).me();
    let mut placement = crate::layout::Placement::new(me, displays.borrow_and_update().clone());
    if let Some((version, author, offsets)) = &group.saved {
        placement.adopt(*version, *author, offsets);
    }
    let mut kept = placement.version();
    let mut target = Target::new(placement.group(), me);
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
    // the peer driving this system while its pointer is here
    let mut driven_by: Option<PublicKey> = None;
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    let mut activity = crate::control::Activity::new(control.physical_activity());
    let mut ended: Vec<(PublicKey, Result<()>)> = Vec::new();
    // a crossing the system this one drives handed on, and whether the
    // arrangement changed, both acted on after the message that caused them
    let mut handoff: Option<(u64, PublicKey, Point)> = None;
    let mut rearranged = false;
    // a copy from a peer was written here, to pass on to the peer this system drives
    let mut fresh_copy = false;
    // links already waiting join before any input is routed
    let mut waiting = waiting;
    waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
    let trace = crate::diagnostics::hub();
    loop {
        control.diagnostics.drain();
        let collecting = trace.collecting();
        for (key, link) in &mut links {
            if link.trace_sent != collecting {
                link.trace_sent = collecting;
                tracing::info!(peer = %key, enabled = collecting, supported = link.trace_capable, "developer trace streaming changed");
                if link.trace_capable {
                    if let Err(error) = link.outgoing.send(Message::TraceControl { enabled: collecting }) {
                        ended.push((*key, Err(error)));
                    }
                } else if collecting {
                    tracing::warn!(peer = %key, "peer does not support developer trace streaming; update this peer");
                }
            }
        }
        for record in trace.drain() {
            for link in links.values_mut().filter(|link| link.trace_request.enabled()) {
                link.outgoing.send_trace(record.clone());
            }
        }
        if placement.version() != kept {
            kept = placement.version();
            let _ = group.save.send(placement.message());
        }
        for (key, result) in ended.drain(..) {
            let Some(link) = links.remove(&key) else { continue };
            link.finish(result);
            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
            let was_owner = state.owner() == key;
            let generation = was_owner.then(|| state.interrupt(control.now()));
            drop(state);
            tracing::debug!(peer = %key, was_owner, generation = ?generation, "peer link ended");
            if was_owner {
                for action in release.target.reclaim() {
                    release.injector.execute(&action);
                }
            }
            if driven_by == Some(key) {
                driven_by = None;
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
            if placement.remove(key) {
                arrange(&placement, release.target, pointer, &group.arranged);
            }
            publish(&control, &links, &group.reports);
        }
        while let Some(change) = waiting.pop_front() {
            match change {
                Membership::Join(joining) => {
                    let key = joining.channel.remote_key();
                    let trace_capable = joining.channel.trace_capable();
                    let protocol = joining.channel.protocol();
                    let files_capable = joining.channel.files_capable();
                    let clipboard_ids = joining.channel.clipboard_ids_capable();
                    let (sender, receiver) = joining.channel.split();
                    let link = Link {
                        protocol,
                        files_capable,
                        clipboard_ids,
                        trace_capable,
                        trace_sent: false,
                        trace_request: crate::diagnostics::hub().lease(),
                        outgoing: spawn_sender(sender),
                        _incoming: spawn_receiver(key, receiver, received_tx.clone()),
                        meter: crate::latency::Meter::default(),
                        last_heard: Instant::now(),
                        nonce: 0,
                        locked: false,
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
                    let introduce = [
                        current,
                        Message::Displays {
                            displays: displays.borrow().clone(),
                        },
                        arrangement(&placement),
                        Message::Locked {
                            locked: *locked.borrow(),
                        },
                    ];
                    if let Err(error) = introduce
                        .into_iter()
                        .try_for_each(|message| link.outgoing.send(message))
                    {
                        link.finish(Err(error));
                    } else if let Some(replaced) = links.insert(key, link) {
                        replaced.finish(Err(Replaced.into()));
                    }
                    publish(&control, &links, &group.reports);
                }
                Membership::Drop(key) => ended.push((key, Err(anyhow::anyhow!("the link was dropped")))),
                Membership::Send(message) => broadcast(&links, message, &mut ended),
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
            _ = trace.changed.notified() => {},
            message = receive_file_message(&mut file_messages) => {
                if let Some(message) = message {
                    for (key, link) in links.iter().filter(|(_, link)| link.files_capable) {
                        if let Message::FilesOffer { offer } = &message
                            && !files.as_ref().is_some_and(|files| files.offered_to(*key, offer.id)) { continue; }
                        if let Err(error) = link.outgoing.send(message.clone()) { ended.push((*key, Err(error))); }
                    }
                }
            },
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
                    driven_by = None;
                }
                if let Some(generation) = claim {
                    for (key, link) in &links {
                        if let Err(error) = link.outgoing.send(Message::ControlClaim { generation }) {
                            ended.push((*key, Err(error)));
                        }
                    }
                    sharing.expect_snapshot(None);
                }
                publish(&control, &links, &group.reports);
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
                log_transition("outgoing", me, None, &message, &control);
                match message {
                    Message::ControlClaim { .. } => {
                        for action in release.target.reclaim() { release.injector.execute(&action); }
                        driven_by = None;
                        sharing.expect_snapshot(None);
                        for (key, link) in &links {
                            if let Err(error) = link.outgoing.send(message.clone()) {
                                ended.push((*key, Err(error)));
                            }
                        }
                    }
                    // a locked system ignores posted input, so the pointer stays here
                    Message::Enter { to, .. } => match links.get(&to).filter(|link| !link.locked) {
                        Some(link) => {
                            crossed = Some(to);
                            if let Err(error) = link.outgoing.send(message) {
                                ended.push((to, Err(error)));
                            } else {
                                if let Some(files) = &files { files.crossing(); }
                                if let Err(error) = hand_clipboard(me, to, link, sharing, files.is_some()) {
                                    ended.push((to, Err(error)));
                                }
                            }
                        }
                        None => {
                        tracing::debug!(destination = %to, "edge entry refused: peer locked or unavailable");
                        pointer.leave(None);
                    },
                    },
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
                log_transition("incoming", me, Some(peer), &message, &control);
                let outcome: Result<()> = (|| {
                    match message {
                        Message::FilesOffer { offer } => {
                            anyhow::ensure!(link.files_capable, "file capability was not negotiated");
                            let id = offer.id;
                            let accepted = files.as_ref().context("copied-file service unavailable").and_then(|files| files.accept(peer, offer));
                            if let Err(error) = accepted {
                                tracing::warn!(peer = %peer, error = format!("{error:#}"), "copied-file offer refused");
                                link.outgoing.send(Message::FilesRelease { offer: id })?;
                            }
                        }
                        Message::FilesRelease { offer } => {
                            anyhow::ensure!(link.files_capable, "file capability was not negotiated");
                            if let Some(files) = &files { files.release(peer, offer); }
                        }
                        Message::TraceControl { enabled } => {
                            anyhow::ensure!(link.trace_capable, "trace capability was not negotiated");
                            link.trace_request.set(enabled);
                            tracing::info!(peer = %peer, enabled, "peer developer trace request changed");
                        }
                        Message::TraceAck { sequence } => {
                            anyhow::ensure!(link.trace_capable, "trace capability was not negotiated");
                            link.outgoing.trace_window.acknowledge(sequence);
                        }
                        Message::TraceRecord { record } => {
                            anyhow::ensure!(link.trace_capable, "trace capability was not negotiated");
                            anyhow::ensure!(record.bounded(), "oversized developer trace record");
                            let sequence = record.sequence;
                            if link.trace_sent && trace.collecting() { trace.collect(peer.to_hex(), record); }
                            link.outgoing.send(Message::TraceAck { sequence })?;
                        }
                        Message::Ping { nonce } => link.outgoing.send(Message::Pong { nonce })?,
                        Message::Pong { nonce } => {
                            if let Some(round_trip) = link.meter.answered(nonce, control.now())
                                && round_trip > crate::latency::SPIKE
                            {
                                tracing::warn!(ms = round_trip.as_millis(), "slow round trip to the peer");
                            }
                        }
                        Message::Clipboard { part } => {
                            if receive_clipboard(peer, part, link, sharing)? {
                                fresh_copy = true;
                            }
                        }
                        Message::ClipboardOffer { copy } => {
                            anyhow::ensure!(link.clipboard_ids, "clipboard ID capability was not negotiated");
                            if sharing.wants(me, peer, copy, driven_by) {
                                link.outgoing.send(Message::ClipboardRequest { copy })?;
                            }
                        }
                        Message::ClipboardRequest { copy } => {
                            anyhow::ensure!(link.clipboard_ids, "clipboard ID capability was not negotiated");
                            link.outgoing.send_clipboard(sharing.requested(me, peer, copy, link.files_capable && files.is_some()));
                        }
                        // the system that chose also sends the arrangement it led to
                        Message::Layout { side, chosen } => {
                            let agreed = crate::control::agreed_side(link.initiator, link.agreed, (side, chosen));
                            if agreed != link.agreed {
                                link.agreed = agreed;
                                let _ = link.agreed_tx.send(agreed);
                            }
                        }
                        Message::Displays { displays } => {
                            let displays = crate::layout::plausible(displays)
                                .context("the peer sent displays that cannot be real")?;
                            if placement.show(peer, displays) {
                                rearranged = true;
                            }
                        }
                        Message::Locked { locked } => link.locked = locked,
                        Message::Activity { generation } => {
                            let accepted = control.state.lock().unwrap_or_else(|e| e.into_inner())
                                .receives_activity(generation, peer, control.now(), *locked.borrow(), control.local_busy());
                            tracing::debug!(peer = %peer, generation, accepted, "display activity decision");
                            if accepted {
                                release.injector.arrived();
                            }
                        }
                        message @ (Message::Introduce { .. } | Message::Revoke { .. }) => {
                            let _ = group.trust.send((peer, message));
                        }
                        Message::Arrangement { version, author, offsets } => {
                            if placement.adopt(version, author, &offsets) {
                                rearranged = true;
                            }
                        }
                        Message::ControlState { generation, owner } => {
                            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let was_mine = state.owns();
                            let changed = state.claim(generation, owner);
                            let mine = state.owns();
                            drop(state);
                            tracing::debug!(peer = %peer, generation, owner = %owner, accepted = changed, "control state decision");
                            if changed && was_mine && !mine {
                                pointer.yield_control();
                                crossed = None;
                                for action in release.target.reclaim() { release.injector.execute(&action); }
                            }
                        }
                        Message::ControlClaim { generation } => {
                            let mut state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let changed = state.claim(generation, peer);
                            let owner = state.owner();
                            let current_generation = state.generation();
                            drop(state);
                            tracing::debug!(peer = %peer, generation, accepted = changed, owner = %owner, current_generation, "control claim decision");
                            if changed {
                                pointer.yield_control();
                                crossed = None;
                                driven_by = None;
                                for action in release.target.reclaim() { release.injector.execute(&action); }
                                if let Some(files) = &files { files.crossing(); }
                                hand_clipboard(me, peer, link, sharing, files.is_some())?;
                            }
                        }
                        Message::Leave { generation, to, at } => {
                            let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let current = state.owns() && state.generation() == generation;
                            drop(state);
                            tracing::debug!(peer = %peer, generation, current, crossed = ?crossed, destination = %to, "edge leave decision");
                            if current && crossed == Some(peer) {
                                crossed = None;
                                handoff = Some((generation, to, at));
                            }
                        }
                        Message::Enter { generation, .. }
                        | Message::Input { generation, .. }
                        | Message::Reclaim { generation } => {
                            let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                            let receives = state.receives(generation, peer, control.now()) && !control.local_busy();
                            drop(state);
                            if !matches!(message, Message::Input { .. }) {
                                tracing::debug!(peer = %peer, generation, receives, local_busy = control.local_busy(), "remote transition decision");
                            } else if !receives {
                                tracing::trace!(peer = %peer, generation, "remote input rejected");
                            }
                            if !receives {
                                if let Message::Enter { at, .. } = message {
                                    // straight back where it came from
                                    let layout = placement.group();
                                    let back = layout.to_shared(me, at).and_then(|at| layout.to_own(peer, at)).unwrap_or(at);
                                    link.outgoing.send(Message::Leave { generation, to: peer, at: back })?;
                                }
                                return Ok(());
                            }
                            let mut crossing = false;
                            let actions = match message {
                                Message::Enter { at, .. } => {
                                    driven_by = Some(peer);
                                    sharing.expect_snapshot(Some(peer));
                                    release.injector.arrived();
                                    release.target.enter(at)
                                },
                                Message::Input { event, .. } => release.target.input(event),
                                Message::Reclaim { .. } => { crossing = true; release.target.reclaim() },
                                _ => unreachable!(),
                            };
                            for action in actions {
                                match action {
                                    Action::Leave { to, at } => { link.outgoing.send(Message::Leave { generation, to, at })?; crossing = true; },
                                    other => release.injector.execute(&other),
                                }
                            }
                            if crossing {
                                driven_by = None;
                                if let Some(files) = &files { files.crossing(); }
                                hand_clipboard(me, peer, link, sharing, files.is_some())?;
                            }
                        }
                        other => bail!("unexpected message in shared session: {other:?}"),
                    }
                    Ok(())
                })();
                if let Err(error) = outcome { ended.push((peer, Err(error))); }
                // the pointer left the system this one drives: home, or on to another
                if let Some((generation, to, at)) = handoff.take() {
                    match links.get(&to).filter(|link| !link.locked) {
                        _ if to == me => {
                            pointer.leave(Some(at));
                            sharing.expect_snapshot(Some(peer));
                        }
                        Some(next) => {
                            crossed = Some(to);
                            // a copy made on the system the pointer just left
                            // follows it here, to be passed on
                            sharing.expect_snapshot(Some(peer));
                            if let Err(error) = next.outgoing.send(Message::Enter { generation, to, at }) {
                                ended.push((to, Err(error)));
                            } else {
                                if let Some(files) = &files { files.crossing(); }
                                if let Err(error) = hand_clipboard(me, to, next, sharing, files.is_some()) {
                                    ended.push((to, Err(error)));
                                }
                            }
                        }
                        None => pointer.leave(None),
                    }
                }
                if std::mem::take(&mut fresh_copy)
                    && let Some(next) = crossed.filter(|next| *next != peer)
                    && let Some(link) = links.get(&next)
                    && let Err(error) = hand_clipboard(me, next, link, sharing, files.is_some())
                {
                    ended.push((next, Err(error)));
                }
                if rearranged {
                    rearranged = false;
                    settle(&mut placement, &links, &mut ended);
                    arrange(&placement, release.target, pointer, &group.arranged);
                }
                publish(&control, &links, &group.reports);
            }
            Ok(()) = choices.changed() => {
                let chosen = *choices.borrow_and_update();
                if let Some((key, offset)) = chosen
                    && placement.put(key, offset, now_ms())
                {
                    broadcast(&links, arrangement(&placement), &mut ended);
                    arrange(&placement, release.target, pointer, &group.arranged);
                }
            }
            Ok(()) = locked.changed() => {
                let now = *locked.borrow_and_update();
                broadcast(&links, Message::Locked { locked: now }, &mut ended);
            }
            Ok(()) = displays.changed() => {
                let mine = displays.borrow_and_update().clone();
                if placement.show(me, mine.clone()) {
                    broadcast(&links, Message::Displays { displays: mine }, &mut ended);
                    if placement.clear_overlaps(now_ms()) {
                        broadcast(&links, arrangement(&placement), &mut ended);
                    }
                    arrange(&placement, release.target, pointer, &group.arranged);
                }
            }
            error = &mut until => return Err(error),
            _ = heartbeat.tick() => {
                let message = {
                    let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
                    activity.take(control.physical_activity(), state.owns(), *locked.borrow())
                        .then(|| Message::Activity { generation: state.generation() })
                };
                if let Some(message) = message {
                    log_transition("outgoing", me, None, &message, &control);
                    broadcast(&links, message, &mut ended);
                }
                for (key, link) in links.iter_mut() {
                    if link.last_heard.elapsed() > SILENCE_LIMIT {
                        tracing::warn!(peer = %key, quiet_ms = link.last_heard.elapsed().as_millis() as u64, "peer heartbeat timed out");
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
                publish(&control, &links, &group.reports);
            }
        }
    }
}

struct FileMessages {
    hub: std::sync::Arc<crate::file_transfer::Hub>,
    receiver: Option<mpsc::Receiver<Message>>,
}

impl Drop for FileMessages {
    fn drop(&mut self) {
        if let Some(receiver) = self.receiver.take() {
            self.hub.put_messages(receiver);
        }
    }
}

async fn receive_file_message(messages: &mut Option<FileMessages>) -> Option<Message> {
    match messages {
        Some(messages) => messages.receiver.as_mut().unwrap().recv().await,
        None => std::future::pending().await,
    }
}

fn log_transition(
    flow: &str,
    me: PublicKey,
    peer: Option<PublicKey>,
    message: &Message,
    control: &crate::control::SharedControl,
) {
    let (reason, generation, destination) = match message {
        Message::ControlClaim { generation } => ("physical_input_claim", Some(*generation), None),
        Message::ControlState { generation, owner } => ("initial_control_state", Some(*generation), Some(*owner)),
        Message::Enter { generation, to, .. } => ("edge_enter", Some(*generation), Some(*to)),
        Message::Leave { generation, to, .. } => ("edge_leave", Some(*generation), Some(*to)),
        Message::Reclaim { generation } => ("emergency_return", Some(*generation), None),
        Message::Activity { generation } => ("display_activity", Some(*generation), None),
        Message::Locked { .. } => ("screen_lock_changed", None, None),
        _ => return,
    };
    // Formatting must not extend the decision lock: capture treats contention
    // as a local-input recovery, so tracing could otherwise induce a jump.
    let (owner, current_generation) = {
        let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.owner(), state.generation())
    };
    tracing::debug!(flow, system = %me, peer = ?peer, reason, generation = ?generation,
        destination = ?destination, owner = %owner, current_generation,
        local_busy = control.local_busy(), "sharing transition");
}

/// Places any member that has no place yet beside this system, on the side
/// agreed with it, and tells every member when that changes the arrangement.
fn settle(
    placement: &mut crate::layout::Placement<PublicKey>,
    links: &std::collections::BTreeMap<PublicKey, Link>,
    ended: &mut Vec<(PublicKey, Result<()>)>,
) {
    let mut placed = false;
    for key in placement.unplaced() {
        if let Some(link) = links.get(&key) {
            placed = placement.place(key, link.agreed.0, now_ms()) || placed;
        }
    }
    if placed {
        broadcast(links, arrangement(placement), ended);
    }
}

fn broadcast(
    links: &std::collections::BTreeMap<PublicKey, Link>,
    message: Message,
    ended: &mut Vec<(PublicKey, Result<()>)>,
) {
    for (key, link) in links {
        if let Err(error) = link.outgoing.send(message.clone()) {
            ended.push((*key, Err(error)));
        }
    }
}

fn arrangement(placement: &crate::layout::Placement<PublicKey>) -> Message {
    let (version, author, offsets) = placement.message();
    Message::Arrangement {
        version,
        author,
        offsets,
    }
}

/// Gives the capture, the replay and the window the arrangement as it now
/// stands.
fn arrange(
    placement: &crate::layout::Placement<PublicKey>,
    target: &mut Target,
    pointer: &mut impl Pointer,
    arranged: &tokio::sync::watch::Sender<Layout>,
) {
    let layout = placement.group();
    target.arrange(layout.clone());
    arranged.send_replace(layout.clone());
    pointer.arrange(layout);
}

/// Milliseconds since the Unix epoch, to order arrangements.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
}

/// Reports each link: its round trips, and who has control.
fn publish(
    control: &crate::control::SharedControl,
    links: &std::collections::BTreeMap<PublicKey, Link>,
    reports: &tokio::sync::watch::Sender<Reports>,
) {
    let state = control.state.lock().unwrap_or_else(|e| e.into_inner());
    let (owner, mine) = (state.owner(), state.owns());
    drop(state);
    let current: Reports = links
        .iter()
        .map(|(key, link)| {
            let report = crate::control::Link {
                protocol: link.protocol,
                latency_ms: link
                    .meter
                    .average()
                    .map(|latency| u64::try_from(latency.as_millis()).unwrap_or(u64::MAX)),
                in_control: mine,
                peer_in_control: owner == *key,
                peer_locked: link.locked,
                stats: link.meter.stats(),
            };
            (*key, report)
        })
        .collect();
    reports.send_if_modified(|reported| {
        let changed = *reported != current;
        *reported = current;
        changed
    });
}

/// Carries out the peer's input on this system.
pub trait Inject {
    fn execute(&mut self, action: &Action);
    /// Control arrived or its owner received new physical input.
    fn arrived(&mut self) {}
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

struct TraceWindow {
    outstanding: std::sync::Mutex<Option<u64>>,
    credit: Semaphore,
}

impl TraceWindow {
    fn acknowledge(&self, sequence: u64) {
        let mut outstanding = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
        if *outstanding == Some(sequence) {
            *outstanding = None;
            self.credit.add_permits(1);
        }
    }
}

struct Outgoing {
    trace_enabled: tokio::sync::watch::Sender<Option<bool>>,
    trace_ack: tokio::sync::watch::Sender<Option<u64>>,
    traces: Option<mpsc::Sender<Message>>,
    trace_dropped: u64,
    trace_window: std::sync::Arc<TraceWindow>,
    messages: Option<mpsc::Sender<Message>>,
    clipboard: Option<mpsc::Sender<Vec<ClipboardPart>>>,
    /// Chunks the peer has acknowledged, and so how many more may be sent.
    window: std::sync::Arc<Semaphore>,
    task: JoinHandle<Result<(), SessionError>>,
}

impl Outgoing {
    fn send_trace(&mut self, mut record: crate::diagnostics::Record) {
        record.dropped_before = record.dropped_before.saturating_add(self.trace_dropped);
        self.trace_dropped = 0;
        if let Some(sender) = &self.traces
            && let Err(error) = sender.try_send(Message::TraceRecord { record })
            && let Message::TraceRecord { record } = error.into_inner()
        {
            self.trace_dropped = record.dropped_before.saturating_add(1);
        }
    }
    fn send(&self, message: Message) -> Result<()> {
        // Diagnostic controls are coalesced separately so they cannot consume
        // input queue slots or turn trace backpressure into a sharing failure.
        match message {
            Message::TraceControl { enabled } => {
                self.trace_enabled.send_replace(Some(enabled));
                return Ok(());
            }
            Message::TraceAck { sequence } => {
                self.trace_ack.send_replace(Some(sequence));
                return Ok(());
            }
            _ => {}
        }
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
        self.traces.take();
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

/// The peer opened a second connection, which took this one's place.
#[derive(Debug, thiserror::Error)]
#[error("the peer connected again")]
pub struct Replaced;

/// Whether a session ended because the connection was lost, or replaced by
/// another, as opposed to either system ending it.
pub fn connection_lost(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Silent>().is_some()
        || error.downcast_ref::<Replaced>().is_some()
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
/// peer's window never stalls. Returns whether a requested copy was written.
fn receive_clipboard(
    from: PublicKey,
    part: ClipboardPart,
    link: &Link,
    sharing: &mut Sharing<impl Clipboard>,
) -> Result<bool> {
    if let ClipboardPart::Ack = part {
        link.outgoing.acknowledged();
        return Ok(false);
    }
    if let ClipboardPart::Chunk { .. } = part {
        link.outgoing.send(Message::Clipboard {
            part: ClipboardPart::Ack,
        })?;
    }
    if link.clipboard_ids {
        return Ok(sharing.receive_requested(from, part).is_some());
    }
    sharing.receive(from, part);
    Ok(false)
}

/// Gives `peer` the clipboard as control or a newer copy reaches it: an
/// offer it requests if it lacks the copy, or for an older peer the snapshot
/// itself unless it already has it.
fn hand_clipboard(
    me: PublicKey,
    peer: PublicKey,
    link: &Link,
    sharing: &mut Sharing<impl Clipboard>,
    files: bool,
) -> Result<()> {
    if link.clipboard_ids {
        if let Some(copy) = sharing.offer(me, peer) {
            link.outgoing.send(Message::ClipboardOffer { copy })?;
        }
    } else {
        link.outgoing
            .send_clipboard(sharing.crossing_for_peer(peer, link.files_capable && files));
    }
    Ok(())
}

fn spawn_sender<W>(mut sender: ChannelSender<W>) -> Outgoing
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (messages, mut outgoing) = mpsc::channel(OUTGOING_CAPACITY);
    let (traces, mut diagnostics) = mpsc::channel(32);
    let (trace_enabled, mut requests) = tokio::sync::watch::channel(None);
    let (trace_ack, mut acknowledgments) = tokio::sync::watch::channel(None);
    let trace_window = std::sync::Arc::new(TraceWindow {
        outstanding: std::sync::Mutex::new(None),
        credit: Semaphore::new(1),
    });
    let credits = trace_window.clone();
    let (clipboard, mut snapshots) = mpsc::channel::<Vec<ClipboardPart>>(CLIPBOARD_CAPACITY);
    let window = std::sync::Arc::new(Semaphore::new(WINDOW));
    let permits = window.clone();
    let task = tokio::spawn(async move {
        let mut snapshots = Some(&mut snapshots);
        let mut pending = VecDeque::new();
        let mut pending_trace = None;
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
                Ok(()) = requests.changed() => {
                    let enabled = *requests.borrow_and_update();
                    if let Some(enabled) = enabled { sender.send(&Message::TraceControl { enabled }).await?; }
                }
                Ok(()) = acknowledgments.changed() => {
                    let sequence = *acknowledgments.borrow_and_update();
                    if let Some(sequence) = sequence { sender.send(&Message::TraceAck { sequence }).await?; }
                }
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
                Some(message) = diagnostics.recv(), if pending_trace.is_none() => pending_trace = Some(message),
                Ok(permit) = credits.credit.acquire(), if pending_trace.is_some() => {
                    permit.forget();
                    let message = pending_trace.take().unwrap();
                    if let Message::TraceRecord { record } = &message {
                        *credits.outstanding.lock().unwrap_or_else(|e| e.into_inner()) = Some(record.sequence);
                    }
                    sender.send(&message).await?;
                }
            }
        }
    });
    Outgoing {
        trace_enabled,
        trace_ack,
        traces: Some(traces),
        trace_dropped: 0,
        trace_window,
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

    /// What every session sends besides what a test looks for.
    fn chatter(message: &Message) -> bool {
        matches!(
            message,
            Message::Ping { .. }
                | Message::ControlState { .. }
                | Message::Displays { .. }
                | Message::Arrangement { .. }
                | Message::Locked { .. }
        )
    }

    /// A key no system in the test has.
    fn someone() -> crate::identity::PublicKey {
        crate::identity::PublicKey::from_bytes(&[7; 32]).unwrap()
    }

    /// This system, as its control knows it.
    fn me_of(control: &SharedControl) -> crate::identity::PublicKey {
        control.state.lock().unwrap().me()
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

    /// A link with a peer that predates clipboard IDs, so snapshots go
    /// straight across as control crosses.
    async fn channels_with_capacity(capacity: usize) -> (Channel<DuplexStream>, Channel<DuplexStream>) {
        let (left, right) = id_channels_with_capacity(capacity).await;
        (left.without_clipboard_ids(), right.without_clipboard_ids())
    }

    /// A link on which clipboard copies go by offer and request.
    async fn id_channels_with_capacity(capacity: usize) -> (Channel<DuplexStream>, Channel<DuplexStream>) {
        let (a, b) = duplex(capacity);
        let (left, right) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let (left, right) = tokio::join!(Channel::initiate(a, &left), Channel::respond(b, &right));
        let (left, right) = (left.unwrap(), right.unwrap());
        assert!(left.clipboard_ids_capable() && right.clipboard_ids_capable());
        (left, right)
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
    struct Returned(Vec<Option<Point>>);

    impl Pointer for Returned {
        fn leave(&mut self, at: Option<Point>) {
            self.0.push(at);
        }
    }

    #[tokio::test]
    async fn each_link_reports_its_round_trips_and_who_has_control() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(2, None).await;
        let (_capture, input) = mpsc::channel(16);
        let (mut injector, mut board, mut pointer) = (Recorded::default(), no_clipboard(), Returned::default());
        let group = group(&control);
        let mut reports = group.reports.subscribe();
        let run = run(
            group,
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let (first, second) = far.split_at_mut(1);
        let answer = async {
            loop {
                if let Message::Ping { nonce } = first[0].recv().await.unwrap() {
                    first[0].send(&Message::Pong { nonce }).await.unwrap();
                }
            }
        };
        // the second member never answers its pings
        let silent = async {
            loop {
                second[0].recv().await.unwrap();
            }
        };
        let measured = async {
            loop {
                reports.changed().await.unwrap();
                let current = reports.borrow_and_update().clone();
                if current.get(&keys[0]).is_some_and(|link| link.latency_ms.is_some()) {
                    break current;
                }
            }
        };
        let reported = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::select! {
                reported = measured => reported,
                () = answer => unreachable!(),
                () = silent => unreachable!(),
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        let answered = reported[&keys[0]];
        assert!(answered.in_control && !answered.peer_in_control);
        assert!(answered.latency_ms.is_some_and(|ms| ms < 1000));
        assert!(answered.stats.count >= 1);
        assert!(answered.stats.p50_us <= answered.stats.max_us);
        let unanswered = reported[&keys[1]];
        assert_eq!((unanswered.latency_ms, unanswered.stats.count), (None, 0));
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
                to: someone(),
                at: (999.0, 123.0),
            })
            .await
            .unwrap();
            loop {
                match peer.recv().await.unwrap() {
                    Message::Leave { generation, at, .. } => break (generation, at),
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
        assert_eq!(returned, (1, (999.0, 123.0)));
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
                to: someone(),
                at: (999.0, 0.0),
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
                to: someone(),
                at: (999.0, 0.0),
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

    /// The peer's offset in each arrangement this system was given.
    #[derive(Default)]
    struct Arranged(Vec<Layout>);

    impl Pointer for Arranged {
        fn leave(&mut self, _at: Option<Point>) {}
        fn arrange(&mut self, layout: Layout) {
            self.0.push(layout);
        }
    }

    /// Where the member other than `me` sits in `layout`.
    fn peer_offset(layout: &Layout, me: crate::identity::PublicKey) -> Option<(f64, f64)> {
        layout
            .members
            .iter()
            .find(|member| member.key != me)
            .map(|member| member.offset)
    }

    struct Scene {
        peer: Channel<DuplexStream>,
        choose: watch::Sender<Option<Placing>>,
        me: crate::identity::PublicKey,
        /// The peer's own key.
        them: crate::identity::PublicKey,
    }

    /// Runs a session that agreed the peer sits on the left, chosen at time
    /// 100, against `script`, which plays the peer.
    async fn arranging<F: Future>(
        initiator: bool,
        script: impl FnOnce(Scene) -> F,
    ) -> (F::Output, Vec<(Side, crate::trust::Timestamp)>, Vec<Option<(f64, f64)>>) {
        let (local, peer) = channels().await;
        let control = control(&local, initiator);
        let me = me_of(&control);
        let them = local.remote_key();
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
                played = script(Scene { peer, choose, me, them }) => played,
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        let mut agreed = Vec::new();
        while let Ok(next) = agreed_rx.try_recv() {
            agreed.push(next);
        }
        let offsets = pointer.0.iter().map(|layout| peer_offset(layout, me)).collect();
        (played, agreed, offsets)
    }

    /// Waits until the session has handled everything sent before.
    async fn settle(peer: &mut Channel<DuplexStream>, nonce: u64) {
        peer.send(&Message::Ping { nonce }).await.unwrap();
        while peer.recv().await.unwrap() != (Message::Pong { nonce }) {}
    }

    /// The next arrangement this system sends.
    async fn next_arrangement(peer: &mut Channel<DuplexStream>) -> Message {
        loop {
            let message = peer.recv().await.unwrap();
            if matches!(message, Message::Arrangement { .. }) {
                return message;
            }
        }
    }

    const LEFT: (f64, f64) = (-1000.0, 0.0);

    #[tokio::test]
    async fn a_peer_that_shows_its_displays_is_placed_on_the_agreed_side() {
        let (sent, _, arranged) = arranging(true, |mut scene: Scene| async move {
            next_arrangement(&mut scene.peer).await;
            scene
                .peer
                .send(&Message::Displays { displays: vec![SCREEN] })
                .await
                .unwrap();
            next_arrangement(&mut scene.peer).await
        })
        .await;
        assert_eq!(arranged.last(), Some(&Some(LEFT)));
        let Message::Arrangement { offsets, .. } = sent else {
            unreachable!()
        };
        assert!(offsets.contains(&(local_key_of(&offsets), LEFT)), "{offsets:?}");
    }

    /// The key in `offsets` that is not at the origin.
    fn local_key_of(offsets: &[(crate::identity::PublicKey, (f64, f64))]) -> crate::identity::PublicKey {
        offsets.iter().find(|(_, offset)| *offset != (0.0, 0.0)).unwrap().0
    }

    #[tokio::test]
    async fn a_newer_arrangement_from_the_peer_moves_the_screens() {
        let ((), agreed, arranged) = arranging(true, |mut scene: Scene| async move {
            let peer_key = scene.them;
            scene
                .peer
                .send(&Message::Displays { displays: vec![SCREEN] })
                .await
                .unwrap();
            let newer = Message::Arrangement {
                version: u64::MAX / 2,
                author: peer_key,
                offsets: vec![(scene.me, (0.0, 0.0)), (peer_key, (1000.0, 0.0))],
            };
            scene.peer.send(&newer).await.unwrap();
            settle(&mut scene.peer, 1).await;
            // an older one never undoes it
            let older = Message::Arrangement {
                version: 1,
                author: peer_key,
                offsets: vec![(scene.me, (0.0, 0.0)), (peer_key, LEFT)],
            };
            scene.peer.send(&older).await.unwrap();
            // a side the peer chose is stored; the peer also sends what it led to
            scene
                .peer
                .send(&Message::Layout {
                    side: Side::Left,
                    chosen: 200,
                })
                .await
                .unwrap();
            settle(&mut scene.peer, 2).await;
        })
        .await;
        assert_eq!(arranged.last(), Some(&Some((1000.0, 0.0))));
        assert_eq!(agreed, [(Side::Right, 200)]);
    }

    #[tokio::test]
    async fn a_member_moved_here_is_sent_to_the_peer() {
        let ((), _, arranged) = arranging(false, |mut scene: Scene| async move {
            scene
                .peer
                .send(&Message::Displays { displays: vec![SCREEN] })
                .await
                .unwrap();
            settle(&mut scene.peer, 1).await;
            scene.choose.send_replace(Some((scene.them, (3.0, -503.0))));
            loop {
                if let Message::Arrangement { offsets, .. } = scene.peer.recv().await.unwrap()
                    && offsets.contains(&(scene.them, (0.0, -500.0)))
                {
                    break;
                }
            }
        })
        .await;
        // dropped near the edge above, and snapped flush to it
        assert_eq!(arranged.last(), Some(&Some((0.0, -500.0))));
    }

    #[tokio::test]
    async fn putting_a_member_where_it_already_is_changes_nothing() {
        let ((), agreed, arranged) = arranging(false, |mut scene: Scene| async move {
            scene
                .peer
                .send(&Message::Displays { displays: vec![SCREEN] })
                .await
                .unwrap();
            settle(&mut scene.peer, 1).await;
            scene.choose.send_replace(Some((scene.them, LEFT)));
            settle(&mut scene.peer, 2).await;
        })
        .await;
        assert!(agreed.is_empty());
        assert_eq!(arranged, [Some(LEFT)]);
    }

    #[tokio::test]
    async fn a_session_that_cannot_choose_still_follows_the_peer() {
        let ((), _, arranged) = arranging(true, |mut scene: Scene| async move {
            drop(scene.choose);
            let peer_key = scene.them;
            scene
                .peer
                .send(&Message::Displays { displays: vec![SCREEN] })
                .await
                .unwrap();
            settle(&mut scene.peer, 1).await;
            let newer = Message::Arrangement {
                version: u64::MAX / 2,
                author: peer_key,
                offsets: vec![(scene.me, (0.0, 0.0)), (peer_key, (0.0, 500.0))],
            };
            scene.peer.send(&newer).await.unwrap();
            settle(&mut scene.peer, 2).await;
        })
        .await;
        assert_eq!(arranged.last(), Some(&Some((0.0, 500.0))));
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
        Vec<crate::identity::PublicKey>,
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
        (control, members, membership, far, ended, keys)
    }

    fn group(control: &Arc<SharedControl>) -> Group {
        Group {
            files: None,
            displays: watch::channel(vec![SCREEN]).1,
            control: control.clone(),
            choices: watch::channel(None).1,
            arranged: Arc::new(watch::Sender::new(Layout { members: Vec::new() })),
            saved: None,
            save: mpsc::unbounded_channel().0,
            reports: Arc::new(watch::Sender::new(Reports::new())),
            locked: watch::channel(false).1,
            trust: mpsc::unbounded_channel().0,
        }
    }

    /// The next message from this system other than heartbeats.
    async fn next(peer: &mut Channel<DuplexStream>) -> Message {
        loop {
            match peer.recv().await.unwrap() {
                Message::Ping { nonce } => peer.send(&Message::Pong { nonce }).await.unwrap(),
                Message::Displays { .. } | Message::Arrangement { .. } | Message::Locked { .. } => {}
                other => return other,
            }
        }
    }

    #[tokio::test]
    async fn every_member_learns_who_has_control_when_it_joins() {
        let (control, _members, mut membership, mut far, _ended, _keys) = group_of(2, Some(0)).await;
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
        let (control, _members, mut membership, mut far, _ended, _keys) = group_of(2, None).await;
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
                    to: someone(),
                    at: (999.0, 0.0),
                })
                .await
                .unwrap();
            // turned straight back
            let back = next(&mut far[1]).await;
            assert!(matches!(back, Message::Leave { generation: 1, .. }), "{back:?}");
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
    async fn the_pointer_is_handed_on_from_one_member_to_the_next() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(2, None).await;
        let me = me_of(&control);
        let (capture, input) = mpsc::channel(16);
        let (mut injector, mut board) = (Recorded::default(), no_clipboard());
        let mut pointer = Returned::default();
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
            let (first, second) = far.split_at_mut(1);
            let (first, second) = (&mut first[0], &mut second[0]);
            for peer in [&mut *first, &mut *second] {
                assert!(matches!(next(peer).await, Message::ControlState { .. }));
            }
            let generation = control.state.lock().unwrap().generation();
            capture
                .send(Message::Enter {
                    generation,
                    to: keys[0],
                    at: (10.0, 20.0),
                })
                .await
                .unwrap();
            assert!(matches!(next(first).await, Message::Enter { at: (10.0, 20.0), .. }));
            // the first sends the pointer on to the second
            first
                .send(&Message::Leave {
                    generation,
                    to: keys[1],
                    at: (5.0, 6.0),
                })
                .await
                .unwrap();
            assert_eq!(
                next(second).await,
                Message::Enter {
                    generation,
                    to: keys[1],
                    at: (5.0, 6.0)
                }
            );
            // a leave from a member that does not have the pointer is ignored
            first
                .send(&Message::Leave {
                    generation,
                    to: me,
                    at: (1.0, 1.0),
                })
                .await
                .unwrap();
            round_trip(first, 1).await;
            // the second sends it home
            second
                .send(&Message::Leave {
                    generation,
                    to: me,
                    at: (7.0, 8.0),
                })
                .await
                .unwrap();
            round_trip(second, 2).await;
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(pointer.0, vec![Some((7.0, 8.0))]);
    }

    #[tokio::test]
    async fn a_display_change_here_reaches_every_member() {
        let (control, _members, mut membership, mut far, _ended, _keys) = group_of(1, None).await;
        let (_capture, input) = mpsc::channel(16);
        let (mut injector, mut board, mut pointer) = (Recorded::default(), no_clipboard(), Returned::default());
        let (show, displays) = watch::channel(vec![SCREEN]);
        let group = Group {
            displays,
            ..group(&control)
        };
        let run = run(
            group,
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let wider = Rect {
            width: 2000.0,
            ..SCREEN
        };
        let script = async {
            let peer = &mut far[0];
            settle(peer, 1).await;
            show.send_replace(vec![SCREEN, wider]);
            loop {
                if let Message::Displays { displays } = peer.recv().await.unwrap()
                    && displays.len() == 2
                {
                    break;
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
    }

    #[tokio::test]
    async fn a_kept_arrangement_places_a_returning_member_and_new_ones_are_kept() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(1, None).await;
        let me = me_of(&control);
        let (_capture, input) = mpsc::channel(16);
        let (mut injector, mut board) = (Recorded::default(), no_clipboard());
        let mut pointer = Arranged::default();
        let (save, mut saved) = mpsc::unbounded_channel();
        let kept = (5, me, vec![(me, (0.0, 0.0)), (keys[0], (0.0, 500.0))]);
        let group = Group {
            saved: Some(kept.clone()),
            save,
            ..group(&control)
        };
        let run = run(
            group,
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            let peer = &mut far[0];
            peer.send(&Message::Displays { displays: vec![SCREEN] }).await.unwrap();
            settle(peer, 1).await;
            // the kept arrangement is offered, not replaced by a fresh placement
            peer.send(&Message::Arrangement {
                version: 9,
                author: keys[0],
                offsets: vec![(me, (0.0, 0.0)), (keys[0], (1000.0, 0.0))],
            })
            .await
            .unwrap();
            settle(peer, 2).await;
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(
            pointer.0.first().and_then(|layout| peer_offset(layout, me)),
            Some((0.0, 500.0)),
            "placed where it was kept"
        );
        let mut versions = Vec::new();
        while let Ok((version, _, _)) = saved.try_recv() {
            versions.push(version);
        }
        assert_eq!(versions, [9], "only the newer arrangement is kept again");
    }

    #[derive(Default)]
    struct Woken(Arc<Mutex<usize>>);

    impl Inject for Woken {
        fn execute(&mut self, _action: &Action) {}
        fn arrived(&mut self) {
            *self.0.lock().unwrap() += 1;
        }
    }

    #[tokio::test]
    async fn control_arriving_wakes_this_system_and_lock_states_are_shared() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(1, None).await;
        let (_capture, input) = mpsc::channel(16);
        let woken = Arc::new(Mutex::new(0));
        let mut injector = Woken(woken.clone());
        let (mut board, mut pointer) = (no_clipboard(), Returned::default());
        let (lock, locked) = watch::channel(false);
        let group = Group {
            locked,
            ..group(&control)
        };
        let mut reports = group.reports.subscribe();
        let run = run(
            group,
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            let peer = &mut far[0];
            peer_enters(peer).await;
            settle(peer, 1).await;
            assert_eq!(*woken.lock().unwrap(), 1);
            // this system locks: the peer hears it
            lock.send_replace(true);
            while peer.recv().await.unwrap() != (Message::Locked { locked: true }) {}
            // the peer locks: this system reports it
            peer.send(&Message::Locked { locked: true }).await.unwrap();
            loop {
                reports.changed().await.unwrap();
                if reports
                    .borrow_and_update()
                    .get(&keys[0])
                    .is_some_and(|link| link.peer_locked)
                {
                    break;
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
    }

    #[tokio::test]
    async fn the_pointer_never_crosses_onto_a_locked_screen() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(1, None).await;
        let (capture, input) = mpsc::channel(16);
        let (mut injector, mut board, mut pointer) = (Recorded::default(), no_clipboard(), Returned::default());
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
            let peer = &mut far[0];
            peer.send(&Message::Locked { locked: true }).await.unwrap();
            settle(peer, 1).await;
            let generation = control.state.lock().unwrap().generation();
            capture
                .send(Message::Enter {
                    generation,
                    to: keys[0],
                    at: (1.0, 1.0),
                })
                .await
                .unwrap();
            let seen = round_trip(peer, 2).await;
            assert!(!seen.iter().any(|m| matches!(m, Message::Enter { .. })), "{seen:?}");
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                () = script => {},
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(pointer.0, vec![None], "the pointer stays where it was");
    }

    #[tokio::test]
    async fn a_second_connection_from_a_member_replaces_the_first_as_lost() {
        let (me, them) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        let connect = || async {
            let (a, b) = duplex(1 << 17);
            let (here, there) = tokio::join!(Channel::initiate(a, &me), Channel::respond(b, &them));
            (here.unwrap(), there.unwrap())
        };
        let join = |channel| {
            let (done, ended) = oneshot::channel();
            let joining = Membership::Join(Joining {
                channel,
                agreed: (Side::Left, 0),
                initiator: true,
                agreed_tx: mpsc::unbounded_channel().0,
                done,
            });
            (joining, ended)
        };
        let control = Arc::new(SharedControl::new(me.public_key(), me.public_key()));
        let (members, mut membership) = mpsc::unbounded_channel();
        let (first, mut far_first) = connect().await;
        let (joining, first_ended) = join(first);
        members.send(joining).ok().unwrap();
        let (_capture, input) = mpsc::channel(16);
        let (mut injector, mut board, mut pointer) = (Recorded::default(), no_clipboard(), Returned::default());
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
            settle(&mut far_first, 1).await;
            let (second, _far_second) = connect().await;
            let (joining, _second_ended) = join(second);
            members.send(joining).ok().unwrap();
            first_ended.await.unwrap()
        };
        let result = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                result = script => result,
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        let error = result.unwrap_err();
        assert!(connection_lost(&error), "{error:#}");
    }

    #[tokio::test]
    async fn losing_the_member_in_control_takes_control_back_and_keeps_the_rest() {
        let (control, _members, mut membership, mut far, mut ended, _keys) = group_of(2, None).await;
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
        // its displays first, so this system knows where the pointer can leave to
        peer.send(&Message::Displays { displays: vec![SCREEN] }).await.unwrap();
        peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
        peer.send(&Message::Enter {
            generation: 1,
            to: someone(),
            at: (999.0, 0.0),
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
                m if chatter(&m) => {}
                other => seen.push(other),
            }
        }
    }

    #[tokio::test]
    async fn replays_input_and_hands_control_back() {
        let (local, mut peer) = channels().await;
        let them = local.remote_key();
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
                to: them,
                at: (999.0, 0.0)
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
        let them = local.remote_key();
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        let mut pointer = Returned::default();
        let mut board = no_clipboard();
        capture
            .try_send(Message::Enter {
                generation: 0,
                to: them,
                at: (999.0, 7.0),
            })
            .unwrap();
        let script = async {
            let mut seen = Vec::new();
            while seen.is_empty() {
                match peer.recv().await.unwrap() {
                    m if chatter(&m) => {}
                    other => seen.push(other),
                }
            }
            peer.send(&Message::Leave {
                generation: 0,
                to: me_of(&control),
                at: (999.0, 9.0),
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
                to: them,
                at: (999.0, 7.0)
            }]
        );
        assert_eq!(pointer.0, vec![Some((999.0, 9.0))]);
    }

    #[tokio::test]
    async fn flushes_accepted_input_when_its_queue_closes() {
        let (local, mut peer) = channels_with_capacity(1).await;
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(2);
        let enter = Message::Enter {
            generation: 0,
            to: local.remote_key(),
            at: (999.0, 7.0),
        };
        capture.try_send(enter.clone()).unwrap();
        drop(capture);
        let receive = async {
            loop {
                match peer.recv().await.unwrap() {
                    m if chatter(&m) => {}
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
                m if chatter(&m) => {}
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
            to: local.remote_key(),
            at: (999.0, 7.0),
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

    /// The next clipboard offer, request or snapshot part from this system,
    /// acknowledging each chunk as a real peer does.
    async fn next_clipboard(peer: &mut Channel<DuplexStream>) -> Message {
        loop {
            match peer.recv().await.unwrap() {
                Message::Ping { nonce } => peer.send(&Message::Pong { nonce }).await.unwrap(),
                message @ (Message::ClipboardOffer { .. } | Message::ClipboardRequest { .. }) => return message,
                Message::Clipboard {
                    part: part @ ClipboardPart::Chunk { .. },
                } => {
                    peer.send(&Message::Clipboard {
                        part: ClipboardPart::Ack,
                    })
                    .await
                    .unwrap();
                    return Message::Clipboard { part };
                }
                message @ Message::Clipboard { .. } => return message,
                _ => {}
            }
        }
    }

    /// Parts up to and including `Done`, after an offer was requested.
    async fn requested_snapshot(peer: &mut Channel<DuplexStream>) -> Vec<Message> {
        let mut seen = Vec::new();
        loop {
            let message = next_clipboard(peer).await;
            let done = message
                == (Message::Clipboard {
                    part: ClipboardPart::Done,
                });
            seen.push(message);
            if done {
                return seen;
            }
        }
    }

    #[tokio::test]
    async fn a_crossing_offers_the_copy_and_sends_it_only_when_asked() {
        let (local, mut peer) = id_channels_with_capacity(1 << 17).await;
        let control = control(&local, true);
        let them = local.remote_key();
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("copied here"), true);
        let (mut pointer, mut injector) = (Returned::default(), Recorded::default());
        let enter = |at| Message::Enter {
            generation: 0,
            to: them,
            at,
        };
        capture.try_send(enter((999.0, 1.0))).unwrap();
        let script = async {
            let Message::ClipboardOffer { copy } = next_clipboard(&mut peer).await else {
                panic!("a crossing offers the copy first");
            };
            peer.send(&Message::ClipboardRequest { copy }).await.unwrap();
            let seen = requested_snapshot(&mut peer).await;
            assert_eq!(
                seen[0],
                Message::Clipboard {
                    part: ClipboardPart::For { copy }
                }
            );
            assert_eq!(assembled(&seen), Some(text("copied here")));
            // control comes back and crosses again: the same copy is offered, and
            // with no request nothing more is sent
            peer.send(&Message::Leave {
                generation: 0,
                to: me_of(&control),
                at: (999.0, 1.0),
            })
            .await
            .unwrap();
            capture.send(enter((999.0, 2.0))).await.unwrap();
            assert_eq!(next_clipboard(&mut peer).await, Message::ClipboardOffer { copy });
            round_trip(&mut peer, 1).await
        };
        let session = together(
            local,
            layout(&control),
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let after = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                after = script => after,
                result = session => panic!("session ended first: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert!(
            !after.iter().any(|m| matches!(m, Message::Clipboard { .. })),
            "{after:?}"
        );
    }

    #[tokio::test]
    async fn a_copy_made_on_a_driven_system_is_passed_on_to_the_next() {
        let (control, _members, mut membership, mut far, _ended, keys) = group_of(2, None).await;
        let me = me_of(&control);
        let (capture, input) = mpsc::channel(16);
        let mut board = no_clipboard();
        let (mut pointer, mut injector) = (Returned::default(), Recorded::default());
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
            let (first, second) = far.split_at_mut(1);
            let (driven, next) = (&mut first[0], &mut second[0]);
            for peer in [&mut *driven, &mut *next] {
                assert!(matches!(super::tests::next(peer).await, Message::ControlState { .. }));
            }
            let generation = control.state.lock().unwrap().generation();
            capture
                .send(Message::Enter {
                    generation,
                    to: keys[0],
                    at: (10.0, 20.0),
                })
                .await
                .unwrap();
            // something is copied on the driven system; the pointer moves on
            let mut copied = board_with(text("copied on the driven system"), true);
            let copy = copied.offer(keys[0], me).unwrap();
            driven
                .send(&Message::Leave {
                    generation,
                    to: keys[1],
                    at: (5.0, 6.0),
                })
                .await
                .unwrap();
            driven.send(&Message::ClipboardOffer { copy }).await.unwrap();
            loop {
                if next_clipboard(driven).await == (Message::ClipboardRequest { copy }) {
                    break;
                }
            }
            for part in copied.requested(keys[0], me, copy, false).map(|read| read()).unwrap() {
                driven.send(&Message::Clipboard { part }).await.unwrap();
            }
            // the next system is offered that copy, asks, and receives it
            loop {
                if next_clipboard(next).await == (Message::ClipboardOffer { copy }) {
                    break;
                }
            }
            next.send(&Message::ClipboardRequest { copy }).await.unwrap();
            requested_snapshot(next).await
        };
        let seen = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! {
                seen = script => seen,
                result = run => panic!("the group ended: {result:?}"),
            }
        })
        .await
        .unwrap();
        assert_eq!(assembled(&seen), Some(text("copied on the driven system")));
    }

    #[tokio::test]
    async fn writes_the_clipboard_that_comes_back() {
        let (local, mut peer) = channels().await;
        let control = control(&local, true);
        let (capture, input) = mpsc::channel(16);
        capture
            .try_send(Message::Enter {
                generation: 0,
                to: local.remote_key(),
                at: (999.0, 3.0),
            })
            .unwrap();
        let mut board = no_clipboard();
        let script = async {
            // the pointer crossed; control comes back, then the peer's clipboard follows
            while !matches!(peer.recv().await.unwrap(), Message::Enter { .. }) {}
            peer.send(&Message::Leave {
                generation: 0,
                to: me_of(&control),
                at: (999.0, 3.0),
            })
            .await
            .unwrap();
            let mut theirs = board_with(text("copied on the peer"), true);
            for part in theirs.crossing(me_of(&control)).map(|read| read()).unwrap_or_default() {
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
            peer.send(&Message::Displays { displays: vec![SCREEN] }).await.unwrap();
            peer.send(&Message::ControlClaim { generation: 1 }).await.unwrap();
            until_snapshot_done(&mut peer).await;
            peer.send(&Message::Enter {
                generation: 1,
                to: someone(),
                at: (999.0, 0.0),
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
        assert!(matches!(seen[0], Message::Leave { generation: 1, .. }), "{:?}", seen[0]);
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
        let them = local.remote_key();
        let control = control(&local, false);
        let (capture, input) = mpsc::channel(16);
        let mut board = board_with(text("stays on this system"), false);
        let script = async {
            peer_enters(&mut peer).await;
            let mut theirs = board_with(text("from the peer"), true);
            for part in theirs.crossing(me_of(&control)).map(|read| read()).unwrap_or_default() {
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
                to: them,
                at: (999.0, 0.0)
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
                to: local.remote_key(),
                at: (999.0, 7.0),
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
                to: local.remote_key(),
                at: (999.0, 7.0),
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

    #[tokio::test(start_paused = true)]
    async fn activity_reaches_every_member_and_stops_when_input_stops() {
        let (control, _members, mut membership, mut far, _ended, _keys) = group_of(2, None).await;
        let (_capture, input) = mpsc::channel(16);
        let (mut pointer, mut injector, mut board) = (Returned::default(), Recorded::default(), no_clipboard());
        let running = run(
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
            for peer in &mut far {
                settle(peer, 100).await;
            }
            control.note_physical();
            for peer in &mut far {
                loop {
                    let message = peer.recv().await.unwrap();
                    if message == (Message::Activity { generation: 0 }) {
                        break;
                    }
                    if let Message::Ping { nonce } = message {
                        peer.send(&Message::Pong { nonce }).await.unwrap();
                    } else {
                        assert!(chatter(&message), "unexpected {message:?}");
                    }
                }
            }
            // Several live heartbeats without input must not refresh idle time.
            for peer in &mut far {
                let mut ticks = 0;
                while ticks < 2 {
                    let message = peer.recv().await.unwrap();
                    assert!(
                        !matches!(message, Message::Activity { .. }),
                        "idle heartbeats sent activity"
                    );
                    if let Message::Ping { nonce } = message {
                        peer.send(&Message::Pong { nonce }).await.unwrap();
                        ticks += 1;
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(8), async {
            tokio::select! { () = script => {}, result = running => panic!("group ended: {result:?}") }
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn activity_messages_obey_owner_generation_and_lock_state() {
        let (control, _members, mut membership, mut far, _ended, _keys) = group_of(2, Some(0)).await;
        let (_capture, input) = mpsc::channel(16);
        let woken = Arc::new(Mutex::new(0));
        let (mut pointer, mut injector, mut board) = (Returned::default(), Woken(woken.clone()), no_clipboard());
        let (lock, locked) = watch::channel(false);
        let group = Group {
            locked,
            ..group(&control)
        };
        let running = run(
            group,
            VecDeque::new(),
            &mut membership,
            input,
            &mut pointer,
            &mut injector,
            &mut board,
            pending(),
        );
        let script = async {
            for peer in &mut far {
                settle(peer, 100).await;
            }
            far[0].send(&Message::Activity { generation: 0 }).await.unwrap();
            settle(&mut far[0], 101).await;
            assert_eq!(
                *woken.lock().unwrap(),
                1,
                "activity must wake a member without an Enter"
            );
            far[1].send(&Message::Activity { generation: 0 }).await.unwrap();
            settle(&mut far[1], 102).await;
            assert_eq!(*woken.lock().unwrap(), 1, "a follower cannot refresh activity");
            far[0].send(&Message::Activity { generation: 1 }).await.unwrap();
            settle(&mut far[0], 103).await;
            assert_eq!(*woken.lock().unwrap(), 1, "wrong generations cannot refresh activity");
            lock.send_replace(true);
            while far[0].recv().await.unwrap() != (Message::Locked { locked: true }) {}
            far[0].send(&Message::Activity { generation: 0 }).await.unwrap();
            settle(&mut far[0], 104).await;
            assert_eq!(*woken.lock().unwrap(), 1, "locked members must stay locked");
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::select! { () = script => {}, result = running => panic!("group ended: {result:?}") }
        })
        .await
        .unwrap();
    }
    fn diagnostic_record(sequence: u64) -> crate::diagnostics::Record {
        crate::diagnostics::Record {
            sequence,
            unix_ms: 0,
            elapsed_ms: 0,
            build: "test".into(),
            level: "TRACE".into(),
            target: "daisy::test".into(),
            fields: Default::default(),
            dropped_before: 0,
        }
    }

    #[tokio::test]
    async fn diagnostic_window_cannot_block_input_or_accept_stale_acknowledgments() {
        let (channel, mut peer) = channels().await;
        let (sender, _receiver) = channel.split();
        let mut outgoing = spawn_sender(sender);
        outgoing.send_trace(diagnostic_record(10));
        outgoing.send_trace(diagnostic_record(11));
        assert!(matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == 10));
        outgoing.trace_window.acknowledge(9);
        outgoing.send(Message::Ping { nonce: 123 }).unwrap();
        assert_eq!(peer.recv().await.unwrap(), Message::Ping { nonce: 123 });
        assert!(
            tokio::time::timeout(Duration::from_millis(25), peer.recv())
                .await
                .is_err()
        );
        outgoing.trace_window.acknowledge(10);
        assert!(matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == 11));
        outgoing.trace_window.acknowledge(10);
        assert_eq!(outgoing.trace_window.credit.available_permits(), 0);
    }

    #[tokio::test]
    async fn saturated_diagnostics_do_not_fill_the_input_queue_and_report_loss() {
        let (channel, mut peer) = channels().await;
        let (sender, _receiver) = channel.split();
        let mut outgoing = spawn_sender(sender);
        for sequence in 0..100 {
            outgoing.send_trace(diagnostic_record(sequence));
        }
        assert_eq!(outgoing.trace_dropped, 68);
        outgoing.send(Message::Reclaim { generation: 17 }).unwrap();
        assert_eq!(peer.recv().await.unwrap(), Message::Reclaim { generation: 17 });
        assert!(matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == 0));
        outgoing.trace_window.acknowledge(0);
        assert!(matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == 1));
        outgoing.send_trace(diagnostic_record(100));
        assert_eq!(outgoing.trace_dropped, 0);
        for sequence in 2..32 {
            outgoing.trace_window.acknowledge(sequence - 1);
            assert!(
                matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == sequence)
            );
        }
        outgoing.trace_window.acknowledge(31);
        assert!(
            matches!(peer.recv().await.unwrap(), Message::TraceRecord { record } if record.sequence == 100 && record.dropped_before == 68)
        );
    }
    #[tokio::test]
    async fn trace_controls_do_not_consume_input_slots_and_coalesce_to_latest_state() {
        let (channel, mut peer) = channels().await;
        let (sender, _receiver) = channel.split();
        let outgoing = spawn_sender(sender);
        for sequence in 0..100 {
            outgoing
                .send(Message::TraceControl {
                    enabled: sequence % 2 == 0,
                })
                .unwrap();
            outgoing.send(Message::TraceAck { sequence }).unwrap();
        }
        assert_eq!(outgoing.messages.as_ref().unwrap().capacity(), OUTGOING_CAPACITY);
        outgoing.send(Message::Ping { nonce: 7 }).unwrap();
        assert_eq!(peer.recv().await.unwrap(), Message::Ping { nonce: 7 });
        assert_eq!(peer.recv().await.unwrap(), Message::TraceControl { enabled: false });
        assert_eq!(peer.recv().await.unwrap(), Message::TraceAck { sequence: 99 });
    }
    #[test]
    fn transition_logging_never_holds_the_capture_decision_lock() {
        use tracing_subscriber::layer::{Context, Layer, SubscriberExt};
        struct CheckLock(Arc<SharedControl>, Arc<std::sync::atomic::AtomicUsize>);
        impl<S: tracing::Subscriber> Layer<S> for CheckLock {
            fn on_event(&self, _: &tracing::Event<'_>, _: Context<'_, S>) {
                assert!(
                    self.0.state.try_lock().is_ok(),
                    "formatting traces must not trigger capture's contention recovery"
                );
                self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let key = Identity::generate().unwrap().public_key();
        let control = Arc::new(SharedControl::new(key, key));
        let emitted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(CheckLock(control.clone(), emitted.clone()));
        tracing::subscriber::with_default(subscriber, || {
            log_transition(
                "outgoing",
                key,
                None,
                &Message::ControlClaim { generation: 1 },
                &control,
            );
        });
        assert_eq!(emitted.load(std::sync::atomic::Ordering::Relaxed), 1);
    }
    #[tokio::test]
    async fn unavailable_file_service_releases_offer_and_keeps_input_link_alive() {
        for service_available in [false, true] {
            let (control, _members, mut membership, mut far, _ended, _keys) = group_of(1, None).await;
            let (_capture, input) = mpsc::channel(16);
            let (mut pointer, mut injector, mut board) = (Returned::default(), Recorded::default(), no_clipboard());
            let home = tempfile::tempdir().unwrap();
            let (_enabled, enabled) = watch::channel(true);
            let (hub, task) = crate::file_transfer::Hub::start(
                Identity::generate().unwrap(),
                crate::device::Signer::generate().unwrap(),
                crate::peers::PeerStore::open(home.path()).unwrap(),
                enabled,
            )
            .unwrap();
            let mut configuration = group(&control);
            if service_available {
                configuration.files = Some(hub);
            }
            let run = run(
                configuration,
                VecDeque::new(),
                &mut membership,
                input,
                &mut pointer,
                &mut injector,
                &mut board,
                pending(),
            );
            let script = async {
                let peer = &mut far[0];
                assert!(matches!(next(peer).await, Message::ControlState { .. }));
                peer.send(&Message::FilesOffer {
                    offer: crate::files::Offer {
                        id: [42; 16],
                        port: if service_available { 0 } else { 1234 },
                        items: vec![crate::files::Item {
                            name: "copied".into(),
                            directory: false,
                            bytes: 1,
                        }],
                    },
                })
                .await
                .unwrap();
                assert_eq!(next(peer).await, Message::FilesRelease { offer: [42; 16] });
                peer.send(&Message::Ping { nonce: 123 }).await.unwrap();
                assert_eq!(next(peer).await, Message::Pong { nonce: 123 });
            };
            tokio::time::timeout(Duration::from_secs(2), async {
            tokio::select! { () = script => {}, result = run => panic!("file offer ended input sharing: {result:?}") }
        })
        .await
        .unwrap();
            task.abort();
        }
    }
}
