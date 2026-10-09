//! Clipboard sharing decisions: what to send when control crosses, how it is
//! split into parts, and how arriving parts are put back together. No macOS
//! calls; `macos::pasteboard` reads and writes the real pasteboard.

use std::collections::HashMap;

use tokio::sync::watch;

use crate::identity::PublicKey;
use crate::protocol::{ClipboardKind, ClipboardPart, CopyId};

/// Largest plain or rich text item sent, in bytes.
pub const MAX_TEXT: usize = 4 << 20;
/// Largest image item sent, in bytes.
pub const MAX_IMAGE: usize = 32 << 20;
/// Bytes per chunk. Small, so a chunk already in the socket delays input only
/// briefly even on a slow link.
pub const CHUNK_LEN: usize = 16_000;
/// Chunks the sender may have unacknowledged. Bounds how much clipboard data
/// can sit ahead of input and heartbeats: `WINDOW * CHUNK_LEN` bytes.
pub const WINDOW: usize = 4;

/// What a clipboard holds, in the forms Daisy carries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Content {
    pub text: Option<String>,
    pub rtf: Option<Vec<u8>>,
    pub png: Option<Vec<u8>>,
    /// Password managers and similar apps mark clipboard items as concealed or
    /// transient. Those items must remain on this system.
    pub concealed: bool,
}

impl Content {
    pub fn is_empty(&self) -> bool {
        self.text.is_none() && self.rtf.is_none() && self.png.is_none()
    }
}

/// The system clipboard. Cloned to read it off the session loop, so cloning
/// must be cheap and every clone must see the same clipboard. `change_count` rises on every change, including
/// Daisy's own writes, which is how an echo is recognised.
pub trait Clipboard: Clone + Send + 'static {
    fn change_count(&self) -> i64;
    fn read(&self) -> Option<Content>;
    /// A peer that receives native files does not also need their text fallback.
    fn read_for_peer(&self, _files: bool) -> Option<Content> {
        self.read()
    }

    /// Replaces the clipboard with `content` and returns the new change count.
    fn write(&mut self, content: &Content) -> i64;
}

fn limit(kind: ClipboardKind) -> usize {
    match kind {
        ClipboardKind::Text | ClipboardKind::Rtf => MAX_TEXT,
        ClipboardKind::Png => MAX_IMAGE,
    }
}

/// Decides whether this system's clipboard goes out to a peer when control
/// crosses to it, and turns it into parts.
#[derive(Debug, Default)]
pub struct Outbox {
    next_id: u32,
    /// The clipboard change each peer already holds: sent to it, with or
    /// without native files, or received from it.
    held: HashMap<PublicKey, Held>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Held {
    count: i64,
    /// Whether it was sent with native files; `None` when it came from that
    /// peer, which holds it in every form.
    files: Option<bool>,
}

impl Outbox {
    /// Parts for the clipboard as it is now, or none when `peer` already
    /// holds it: sent there unchanged, or what arrived from there.
    pub fn take(&mut self, peer: PublicKey, clipboard: &impl Clipboard) -> Vec<ClipboardPart> {
        match self.prepare_for_peer(peer, clipboard.change_count(), false) {
            Some(first) => clipboard.read().map(|c| parts(first, &c)).unwrap_or_default(),
            None => Vec::new(),
        }
    }

    /// Whether `peer` lacks the clipboard at `count`; if so, records it as
    /// sent there and reserves the ids its items will use.
    fn prepare_for_peer(&mut self, peer: PublicKey, count: i64, files: bool) -> Option<u32> {
        let has = |held: &Held| held.count == count && held.files.is_none_or(|sent| sent == files);
        if self.held.get(&peer).is_some_and(has) {
            return None;
        }
        self.held.insert(
            peer,
            Held {
                count,
                files: Some(files),
            },
        );
        Some(self.reserve())
    }

    /// Reserves the item ids one snapshot uses.
    fn reserve(&mut self) -> u32 {
        let first = self.next_id;
        self.next_id = self.next_id.wrapping_add(ITEM_KINDS);
        first
    }

    /// Records writing `from`'s clipboard, so it is not sent straight back.
    pub fn wrote(&mut self, from: PublicKey, change_count: i64) {
        self.held.insert(
            from,
            Held {
                count: change_count,
                files: None,
            },
        );
    }
}

/// Item kinds a snapshot can hold, and so ids reserved per snapshot.
const ITEM_KINDS: u32 = 3;

/// A snapshot's parts, with item ids starting at `first`. Concealed content
/// and items over their limit are left out.
fn parts(first: u32, content: &Content) -> Vec<ClipboardPart> {
    if content.concealed {
        tracing::info!("concealed clipboard item was not shared");
        return Vec::new();
    }
    let items = [
        (ClipboardKind::Text, content.text.as_ref().map(|t| t.as_bytes())),
        (ClipboardKind::Rtf, content.rtf.as_deref()),
        (ClipboardKind::Png, content.png.as_deref()),
    ];
    let mut out = Vec::new();
    for (offset, (kind, bytes)) in items.into_iter().enumerate() {
        let Some(bytes) = bytes else { continue };
        if bytes.len() > limit(kind) {
            tracing::info!(?kind, len = bytes.len(), "clipboard item too large to share");
            continue;
        }
        let id = first.wrapping_add(offset as u32);
        out.push(ClipboardPart::Begin {
            id,
            kind,
            len: bytes.len() as u32,
        });
        out.extend(bytes.chunks(CHUNK_LEN).map(|chunk| ClipboardPart::Chunk {
            id,
            bytes: chunk.to_vec(),
        }));
        out.push(ClipboardPart::End { id });
    }
    if !out.is_empty() {
        out.push(ClipboardPart::Done);
    }
    out
}

struct Pending {
    kind: ClipboardKind,
    len: usize,
    bytes: Vec<u8>,
}

/// Puts arriving parts back together. A malformed or oversized item is
/// dropped on its own; the rest of the snapshot still arrives.
#[derive(Default)]
pub struct Inbox {
    pending: HashMap<u32, Pending>,
    done: Content,
}

impl Inbox {
    /// Feeds one part; returns the snapshot when `Done` completes it.
    pub fn accept(&mut self, part: ClipboardPart) -> Option<Content> {
        match part {
            ClipboardPart::Begin { id, kind, len } => {
                let len = len as usize;
                // One snapshot has at most one value of each kind. Replacing a
                // prior declaration bounds pending memory even for a buggy or
                // hostile peer that sends Begin repeatedly without Done.
                self.pending.retain(|_, item| item.kind != kind);
                if len <= limit(kind) {
                    self.pending.insert(
                        id,
                        Pending {
                            kind,
                            len,
                            bytes: Vec::with_capacity(len.min(CHUNK_LEN)),
                        },
                    );
                }
            }
            ClipboardPart::Chunk { id, bytes } => {
                if let Some(item) = self.pending.get_mut(&id) {
                    if item.bytes.len() + bytes.len() > item.len {
                        self.pending.remove(&id);
                    } else {
                        item.bytes.extend_from_slice(&bytes);
                    }
                }
            }
            ClipboardPart::End { id } => {
                if let Some(item) = self.pending.remove(&id)
                    && item.bytes.len() == item.len
                {
                    match item.kind {
                        ClipboardKind::Text => self.done.text = String::from_utf8(item.bytes).ok(),
                        ClipboardKind::Rtf => self.done.rtf = Some(item.bytes),
                        ClipboardKind::Png => self.done.png = Some(item.bytes),
                    }
                }
            }
            // acknowledgements are for the sender, and a copy's name is read
            // before its parts reach here; neither is part of the content
            ClipboardPart::Ack | ClipboardPart::For { .. } => {}
            ClipboardPart::Done => {
                self.pending.clear();
                let content = std::mem::take(&mut self.done);
                return (!content.is_empty()).then_some(content);
            }
        }
        None
    }
}

/// One side of clipboard sharing in a session: this system's clipboard,
/// what it has sent and received, and whether sharing is switched on.
pub struct Sharing<C> {
    clipboard: C,
    enabled: watch::Receiver<bool>,
    outbox: Outbox,
    inbox: Inbox,
    /// Control just crossed to this system, so one snapshot is due: from this
    /// peer, or from whichever peer had control when that is not known.
    expecting: Option<Option<PublicKey>>,
    /// A due snapshot is arriving from this peer; parts from any other are
    /// ignored until it is done.
    receiving: Option<PublicKey>,
    /// With clipboard IDs: the copy on the pasteboard when it came from a
    /// peer, and the change count writing it produced.
    received: Option<(CopyId, i64)>,
    /// The copy last offered to each peer; a request is answered only for it.
    offered: HashMap<PublicKey, CopyId>,
    /// The copy last requested, from whom, and whether its snapshot has begun.
    requested: Option<(PublicKey, CopyId, bool)>,
    requested_inbox: Inbox,
}

impl<C: Clipboard> Sharing<C> {
    pub fn new(clipboard: C, enabled: watch::Receiver<bool>) -> Self {
        Self {
            clipboard,
            enabled,
            outbox: Outbox::default(),
            inbox: Inbox::default(),
            expecting: None,
            receiving: None,
            received: None,
            offered: HashMap::new(),
            requested: None,
            requested_inbox: Inbox::default(),
        }
    }

    pub fn clipboard(&self) -> &C {
        &self.clipboard
    }

    /// Control is leaving this system for `peer`. Returns the work of reading
    /// the clipboard and splitting it into parts, to run off the session loop:
    /// a large image takes long enough to read and convert that input would
    /// stall behind it.
    pub fn crossing(&mut self, peer: PublicKey) -> Option<impl FnOnce() -> Vec<ClipboardPart> + Send + 'static> {
        self.crossing_for_peer(peer, false)
    }

    pub fn crossing_for_peer(
        &mut self,
        peer: PublicKey,
        files: bool,
    ) -> Option<impl FnOnce() -> Vec<ClipboardPart> + Send + 'static> {
        if !*self.enabled.borrow() {
            return None;
        }
        let first = self
            .outbox
            .prepare_for_peer(peer, self.clipboard.change_count(), files)?;
        let clipboard = self.clipboard.clone();
        Some(move || {
            clipboard
                .read_for_peer(files)
                .map(|c| parts(first, &c))
                .unwrap_or_default()
        })
    }

    /// Control has crossed to this system; the peer's clipboard follows.
    /// The copy on the pasteboard now: the one received from a peer while
    /// the pasteboard still holds it, otherwise a copy made here.
    pub fn current(&self, me: PublicKey) -> CopyId {
        let count = self.clipboard.change_count();
        match self.received {
            Some((copy, written)) if written == count => copy,
            _ => CopyId { origin: me, count },
        }
    }

    /// The copy to offer `peer` as control crosses to it or a newer copy
    /// arrives here, or none with sharing switched off.
    pub fn offer(&mut self, me: PublicKey, peer: PublicKey) -> Option<CopyId> {
        if !*self.enabled.borrow() {
            return None;
        }
        let copy = self.current(me);
        self.offered.insert(peer, copy);
        Some(copy)
    }

    /// Whether to ask `from` for the copy it offered. Only an offer from the
    /// peer control just crossed here from, or from the peer `driver` now
    /// driving this system, is considered, and only a copy not already here.
    pub fn wants(&mut self, me: PublicKey, from: PublicKey, copy: CopyId, driver: Option<PublicKey>) -> bool {
        let expected = self
            .expecting
            .is_some_and(|sender| sender.is_none_or(|sender| sender == from));
        if expected {
            self.expecting = None;
        }
        if !(expected || driver == Some(from)) || !*self.enabled.borrow() || copy == self.current(me) {
            return false;
        }
        self.requested = Some((from, copy, false));
        self.requested_inbox = Inbox::default();
        true
    }

    /// The work of reading the copy `peer` asked for, if it was offered to
    /// `peer` and is still on the pasteboard. `files` as for crossings.
    pub fn requested(
        &mut self,
        me: PublicKey,
        peer: PublicKey,
        copy: CopyId,
        files: bool,
    ) -> Option<impl FnOnce() -> Vec<ClipboardPart> + Send + 'static> {
        if !*self.enabled.borrow() || self.offered.get(&peer) != Some(&copy) || self.current(me) != copy {
            return None;
        }
        let count = self.clipboard.change_count();
        let first = self.outbox.reserve();
        let clipboard = self.clipboard.clone();
        Some(move || {
            // a copy made after the request is offered on its own
            if clipboard.change_count() != count {
                return Vec::new();
            }
            let content = clipboard
                .read_for_peer(files)
                .map(|c| parts(first, &c))
                .unwrap_or_default();
            if content.is_empty() {
                return content;
            }
            std::iter::once(ClipboardPart::For { copy }).chain(content).collect()
        })
    }

    /// A part of a requested snapshot from `from`. Only the snapshot `For`
    /// the copy last requested from `from` is accepted. Returns the copy once
    /// it is written here.
    pub fn receive_requested(&mut self, from: PublicKey, part: ClipboardPart) -> Option<CopyId> {
        let (sender, copy, begun) = self.requested?;
        if sender != from {
            return None;
        }
        if let ClipboardPart::For { copy: named } = part {
            if named == copy && !begun {
                self.requested = Some((sender, copy, true));
            }
            return None;
        }
        if !begun {
            return None;
        }
        let done = matches!(part, ClipboardPart::Done);
        let written = self.requested_inbox.accept(part).and_then(|content| {
            if !*self.enabled.borrow() {
                return None;
            }
            let count = self.clipboard.write(&content);
            self.outbox.wrote(from, count);
            self.received = Some((copy, count));
            Some(copy)
        });
        if done {
            self.requested = None;
        }
        written
    }

    /// Control crossed here from `from`, or from an unknown peer when it was
    /// taken by using this system; that peer's clipboard follows.
    pub fn expect_snapshot(&mut self, from: Option<PublicKey>) {
        self.expecting = Some(from);
        // a snapshot still arriving from an earlier crossing never finished,
        // perhaps because its sender disconnected; it must not block this one
        self.receiving = None;
        self.inbox = Inbox::default();
    }

    /// A part arrived from the peer. Only a snapshot that follows a crossing
    /// to this system is accepted, so the peer cannot replace this clipboard at
    /// other times. A completed one is written if sharing is on.
    pub fn receive(&mut self, from: PublicKey, part: ClipboardPart) {
        match self.receiving {
            Some(sender) if sender != from => return,
            Some(_) => {}
            None => {
                let due = self
                    .expecting
                    .is_some_and(|sender| sender.is_none_or(|sender| sender == from));
                if !(due && matches!(part, ClipboardPart::Begin { .. })) {
                    return;
                }
                self.expecting = None;
                self.receiving = Some(from);
            }
        }
        let done = matches!(part, ClipboardPart::Done);
        if let Some(content) = self.inbox.accept(part)
            && *self.enabled.borrow()
        {
            let count = self.clipboard.write(&content);
            self.outbox.wrote(from, count);
        }
        if done {
            self.receiving = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn system(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    /// The peer an exchange in these tests goes to or comes from.
    fn peer() -> PublicKey {
        system(9)
    }

    #[derive(Default, Clone)]
    struct Fake {
        count: i64,
        content: Option<Content>,
        file_text: bool,
    }

    impl Fake {
        fn copy(&mut self, content: Content) {
            self.count += 1;
            self.content = Some(content);
        }
    }

    impl Clipboard for Fake {
        fn change_count(&self) -> i64 {
            self.count
        }
        fn read(&self) -> Option<Content> {
            self.content.clone()
        }
        fn read_for_peer(&self, files: bool) -> Option<Content> {
            if files && self.file_text { None } else { self.read() }
        }
        fn write(&mut self, content: &Content) -> i64 {
            self.copy(content.clone());
            self.count
        }
    }

    fn text(s: &str) -> Content {
        Content {
            text: Some(s.into()),
            ..Content::default()
        }
    }

    fn deliver(parts: Vec<ClipboardPart>) -> Option<Content> {
        let mut inbox = Inbox::default();
        let mut result = None;
        for part in parts {
            if let Some(content) = inbox.accept(part) {
                result = Some(content);
            }
        }
        result
    }

    #[test]
    fn text_rich_text_and_images_arrive_whole() {
        let content = Content {
            text: Some("héllo".into()),
            rtf: Some(b"{\\rtf1 hello}".to_vec()),
            png: Some(vec![7; CHUNK_LEN * 2 + 1]),
            concealed: false,
        };
        let mut clipboard = Fake::default();
        clipboard.copy(content.clone());
        let parts = Outbox::default().take(peer(), &clipboard);
        let chunks = parts
            .iter()
            .filter(|p| matches!(p, ClipboardPart::Chunk { .. }))
            .count();
        assert_eq!(chunks, 1 + 1 + 3);
        assert_eq!(deliver(parts), Some(content));
    }

    #[test]
    fn items_at_the_limit_are_sent_and_larger_ones_left_out() {
        let mut clipboard = Fake::default();
        clipboard.copy(Content {
            text: Some("a".repeat(MAX_TEXT)),
            rtf: Some(vec![b'x'; MAX_TEXT + 1]),
            png: None,
            concealed: false,
        });
        let arrived = deliver(Outbox::default().take(peer(), &clipboard)).unwrap();
        assert_eq!(arrived.text.map(|t| t.len()), Some(MAX_TEXT));
        assert_eq!(arrived.rtf, None);
    }

    #[test]
    fn nothing_is_sent_when_every_item_is_too_large() {
        let mut clipboard = Fake::default();
        clipboard.copy(Content {
            png: Some(vec![0; MAX_IMAGE + 1]),
            ..Content::default()
        });
        assert!(Outbox::default().take(peer(), &clipboard).is_empty());
    }

    #[test]
    fn concealed_items_are_never_sent() {
        let mut clipboard = Fake::default();
        clipboard.copy(Content {
            text: Some("secret".into()),
            concealed: true,
            ..Content::default()
        });
        assert!(Outbox::default().take(peer(), &clipboard).is_empty());
    }

    #[test]
    fn an_unchanged_clipboard_is_not_sent_again() {
        let mut clipboard = Fake::default();
        clipboard.copy(text("one"));
        let mut outbox = Outbox::default();
        assert!(!outbox.take(peer(), &clipboard).is_empty());
        assert!(outbox.take(peer(), &clipboard).is_empty());
        clipboard.copy(text("two"));
        assert_eq!(deliver(outbox.take(peer(), &clipboard)), Some(text("two")));
    }

    #[test]
    fn what_arrived_from_the_peer_is_not_sent_back() {
        let mut clipboard = Fake::default();
        let mut outbox = Outbox::default();
        let count = clipboard.write(&text("from the peer"));
        outbox.wrote(peer(), count);
        assert!(outbox.take(peer(), &clipboard).is_empty());
        clipboard.copy(text("copied here afterwards"));
        assert_eq!(
            deliver(outbox.take(peer(), &clipboard)),
            Some(text("copied here afterwards"))
        );
    }

    #[test]
    fn an_empty_clipboard_sends_nothing() {
        let clipboard = Fake {
            count: 3,
            ..Fake::default()
        };
        assert!(Outbox::default().take(peer(), &clipboard).is_empty());
    }

    #[test]
    fn a_whole_item_over_the_limit_is_refused() {
        let bytes = vec![b'a'; MAX_TEXT + 1];
        let mut parts = vec![ClipboardPart::Begin {
            id: 1,
            kind: ClipboardKind::Text,
            len: bytes.len() as u32,
        }];
        parts.extend(bytes.chunks(CHUNK_LEN).map(|c| ClipboardPart::Chunk {
            id: 1,
            bytes: c.to_vec(),
        }));
        parts.extend([ClipboardPart::End { id: 1 }, ClipboardPart::Done]);
        assert_eq!(deliver(parts), None);
    }

    #[test]
    fn an_item_is_discarded_as_soon_as_it_outgrows_its_length() {
        let mut inbox = Inbox::default();
        inbox.accept(ClipboardPart::Begin {
            id: 1,
            kind: ClipboardKind::Png,
            len: 2,
        });
        inbox.accept(ClipboardPart::Chunk {
            id: 1,
            bytes: vec![0; 3],
        });
        assert!(inbox.pending.is_empty());
    }

    #[test]
    fn an_item_longer_or_shorter_than_declared_is_dropped() {
        let item = |id, len, bytes: &[u8]| {
            vec![
                ClipboardPart::Begin {
                    id,
                    kind: ClipboardKind::Rtf,
                    len,
                },
                ClipboardPart::Chunk {
                    id,
                    bytes: bytes.to_vec(),
                },
                ClipboardPart::End { id },
            ]
        };
        let mut parts = item(1, 2, b"abc");
        parts.extend(item(2, 4, b"abc"));
        parts.push(ClipboardPart::Done);
        assert_eq!(deliver(parts), None);
    }

    #[test]
    fn invalid_utf8_text_is_dropped_but_the_rest_arrives() {
        let parts = vec![
            ClipboardPart::Begin {
                id: 1,
                kind: ClipboardKind::Text,
                len: 2,
            },
            ClipboardPart::Chunk {
                id: 1,
                bytes: vec![0xff, 0xfe],
            },
            ClipboardPart::End { id: 1 },
            ClipboardPart::Begin {
                id: 2,
                kind: ClipboardKind::Rtf,
                len: 1,
            },
            ClipboardPart::Chunk {
                id: 2,
                bytes: vec![b'x'],
            },
            ClipboardPart::End { id: 2 },
            ClipboardPart::Done,
        ];
        assert_eq!(
            deliver(parts),
            Some(Content {
                rtf: Some(vec![b'x']),
                ..Content::default()
            })
        );
    }

    #[test]
    fn chunks_for_an_unknown_item_are_ignored() {
        let parts = vec![
            ClipboardPart::Chunk { id: 9, bytes: vec![1] },
            ClipboardPart::End { id: 9 },
            ClipboardPart::Done,
        ];
        assert_eq!(deliver(parts), None);
    }

    #[test]
    fn repeated_begins_keep_only_one_pending_item_per_kind() {
        let mut inbox = Inbox::default();
        for id in 0..100 {
            assert_eq!(
                inbox.accept(ClipboardPart::Begin {
                    id,
                    kind: ClipboardKind::Png,
                    len: MAX_IMAGE as u32,
                }),
                None
            );
        }
        assert_eq!(inbox.pending.len(), 1);
        assert!(inbox.pending.contains_key(&99));
    }

    #[test]
    fn a_text_only_copy_drops_the_previous_rich_text() {
        let mut inbox = Inbox::default();
        let mut outbox = Outbox::default();
        let mut clipboard = Fake::default();
        let receive = |inbox: &mut Inbox, parts: Vec<ClipboardPart>| parts.into_iter().find_map(|p| inbox.accept(p));
        clipboard.copy(Content {
            text: Some("styled".into()),
            rtf: Some(b"{\\rtf1 styled}".to_vec()),
            png: None,
            concealed: false,
        });
        assert!(
            receive(&mut inbox, outbox.take(peer(), &clipboard))
                .unwrap()
                .rtf
                .is_some()
        );
        clipboard.copy(text("plain"));
        assert_eq!(
            receive(&mut inbox, outbox.take(peer(), &clipboard)),
            Some(text("plain"))
        );
    }

    fn sharing(content: Option<Content>) -> (Sharing<Fake>, watch::Sender<bool>) {
        let (on, enabled) = watch::channel(true);
        let mut clipboard = Fake::default();
        if let Some(content) = content {
            clipboard.copy(content);
        }
        (Sharing::new(clipboard, enabled), on)
    }

    /// The parts this system would send as control leaves it.
    fn outgoing(sharing: &mut Sharing<Fake>) -> Vec<ClipboardPart> {
        sharing.crossing(peer()).map(|read| read()).unwrap_or_default()
    }

    #[test]
    fn a_copy_crosses_and_is_not_echoed_back() {
        let (mut here, _on_here) = sharing(Some(text("copied here")));
        let (mut there, _on_there) = sharing(None);
        there.expect_snapshot(None);
        for part in outgoing(&mut here) {
            there.receive(peer(), part);
        }
        assert_eq!(there.clipboard.content, Some(text("copied here")));
        assert!(outgoing(&mut there).is_empty());
    }

    #[test]
    fn switched_off_nothing_is_sent_or_written() {
        let (mut here, on_here) = sharing(Some(text("secret")));
        on_here.send_replace(false);
        assert!(outgoing(&mut here).is_empty());
        let (mut other, _on) = sharing(Some(text("from the peer")));
        here.expect_snapshot(None);
        for part in outgoing(&mut other) {
            here.receive(peer(), part);
        }
        assert_eq!(here.clipboard.content, Some(text("secret")));
    }

    #[test]
    fn a_snapshot_that_does_not_follow_a_crossing_is_ignored() {
        let (mut here, _on) = sharing(Some(text("mine")));
        let (mut other, _on_other) = sharing(Some(text("unsolicited")));
        for part in outgoing(&mut other) {
            here.receive(peer(), part);
        }
        assert_eq!(here.clipboard.content, Some(text("mine")));
    }

    #[test]
    fn only_one_snapshot_is_accepted_per_crossing() {
        let (mut here, _on) = sharing(Some(text("mine")));
        let (mut other, _on_other) = sharing(Some(text("first")));
        here.expect_snapshot(None);
        for part in outgoing(&mut other) {
            here.receive(peer(), part);
        }
        other.clipboard.copy(text("second"));
        for part in outgoing(&mut other) {
            here.receive(peer(), part);
        }
        assert_eq!(here.clipboard.content, Some(text("first")));
    }

    #[test]
    fn nothing_is_read_until_the_work_is_run() {
        let (mut here, _on) = sharing(Some(text("copied")));
        let read = here.crossing(peer()).expect("a new copy is due");
        // the copy is already counted as sent; running the work later still sends it
        assert!(here.crossing(peer()).is_none());
        assert!(!read().is_empty());
    }
    #[test]
    fn file_text_fallback_follows_destination_capability_without_losing_snapshot() {
        let mut clipboard = Fake {
            file_text: true,
            ..Fake::default()
        };
        clipboard.copy(text("copied-file.txt"));
        let (_on, enabled) = watch::channel(true);
        let mut sharing = Sharing::new(clipboard, enabled);
        assert!(sharing.crossing_for_peer(peer(), true).unwrap()().is_empty());
        assert_eq!(
            deliver(sharing.crossing_for_peer(peer(), false).unwrap()()),
            Some(text("copied-file.txt"))
        );
        assert!(sharing.crossing_for_peer(peer(), false).is_none());
        assert!(sharing.crossing_for_peer(peer(), true).unwrap()().is_empty());
    }

    #[test]
    fn a_copy_reaches_every_peer_control_visits() {
        let (mut here, _on) = sharing(Some(text("copied here")));
        let (first, second) = (system(1), system(2));
        let to = |here: &mut Sharing<Fake>, peer| deliver(here.crossing(peer).map(|read| read()).unwrap_or_default());
        assert_eq!(to(&mut here, first), Some(text("copied here")));
        assert_eq!(
            to(&mut here, second),
            Some(text("copied here")),
            "the next peer also needs it"
        );
        assert_eq!(to(&mut here, first), None, "the first peer already has it");
    }

    #[test]
    fn a_copy_received_is_passed_on_but_not_sent_back() {
        let (origin, next) = (system(1), system(2));
        let (mut there, _on_there) = sharing(Some(text("copied on the first system")));
        let (mut here, _on_here) = sharing(None);
        here.expect_snapshot(None);
        for part in there.crossing(system(3)).map(|read| read()).unwrap_or_default() {
            here.receive(origin, part);
        }
        assert_eq!(here.clipboard.content, Some(text("copied on the first system")));
        assert!(here.crossing(origin).is_none(), "not echoed to where it came from");
        assert_eq!(
            deliver(here.crossing(next).map(|read| read()).unwrap_or_default()),
            Some(text("copied on the first system")),
            "passed on to the next system"
        );
    }

    #[test]
    fn a_snapshot_belongs_to_the_peer_that_began_it() {
        let (sender, intruder, next) = (system(1), system(2), system(3));
        let (mut there, _on_there) = sharing(Some(text("copied on the sender")));
        let (mut other, _on_other) = sharing(Some(text("from someone else")));
        let (mut here, _on_here) = sharing(None);
        here.expect_snapshot(None);
        let mut theirs = there.crossing(system(4)).map(|read| read()).unwrap_or_default();
        let done = theirs.pop().unwrap();
        for part in theirs {
            here.receive(sender, part);
        }
        for part in other.crossing(system(4)).map(|read| read()).unwrap_or_default() {
            here.receive(intruder, part);
        }
        assert_eq!(
            here.clipboard.content, None,
            "another peer cannot finish or feed the snapshot"
        );
        here.receive(sender, done);
        assert_eq!(here.clipboard.content, Some(text("copied on the sender")));
        assert!(here.crossing(sender).is_none(), "not echoed to the peer it came from");
        assert!(here.crossing(intruder).is_some(), "the other peer does not have it");
        assert!(here.crossing(next).is_some());
    }

    #[test]
    fn a_new_crossing_replaces_a_snapshot_that_never_finished() {
        let (gone, next) = (system(1), system(2));
        let (mut there, _on_there) = sharing(Some(text("never finished")));
        let (mut other, _on_other) = sharing(Some(text("copied on the next peer")));
        let (mut here, _on_here) = sharing(None);
        here.expect_snapshot(None);
        let mut unfinished = there.crossing(system(4)).map(|read| read()).unwrap_or_default();
        unfinished.pop();
        for part in unfinished {
            here.receive(gone, part);
        }
        // the first peer disconnected; control later crosses here from another
        here.expect_snapshot(None);
        for part in other.crossing(system(4)).map(|read| read()).unwrap_or_default() {
            here.receive(next, part);
        }
        assert_eq!(here.clipboard.content, Some(text("copied on the next peer")));
    }

    #[test]
    fn a_late_snapshot_from_another_peer_cannot_take_a_crossing_s_place() {
        let (entered_from, late) = (system(1), system(2));
        let (mut there, _on_there) = sharing(Some(text("from the peer control came from")));
        let (mut other, _on_other) = sharing(Some(text("late from someone else")));
        let (mut here, _on_here) = sharing(None);
        here.expect_snapshot(Some(entered_from));
        for part in other.crossing(system(4)).map(|read| read()).unwrap_or_default() {
            here.receive(late, part);
        }
        for part in there.crossing(system(4)).map(|read| read()).unwrap_or_default() {
            here.receive(entered_from, part);
        }
        assert_eq!(here.clipboard.content, Some(text("from the peer control came from")));
    }

    /// One system in a clipboard-ID group.
    struct Member {
        key: PublicKey,
        sharing: Sharing<Fake>,
        _on: watch::Sender<bool>,
    }

    fn member(byte: u8, content: Option<Content>) -> Member {
        let (sharing, on) = sharing(content);
        Member {
            key: system(byte),
            sharing,
            _on: on,
        }
    }

    /// `from` offers its copy to `to`, which control just reached from
    /// `from` (or which `from` drives); returns the bytes of clipboard data
    /// sent, zero when `to` already had the copy.
    fn hand(from: &mut Member, to: &mut Member, driven: bool) -> usize {
        if !driven {
            to.sharing.expect_snapshot(Some(from.key));
        }
        let Some(copy) = from.sharing.offer(from.key, to.key) else {
            return 0;
        };
        let driver = driven.then_some(from.key);
        if !to.sharing.wants(to.key, from.key, copy, driver) {
            return 0;
        }
        let parts = from
            .sharing
            .requested(from.key, to.key, copy, false)
            .map(|read| read())
            .unwrap_or_default();
        let sent = parts
            .iter()
            .map(|part| match part {
                ClipboardPart::Chunk { bytes, .. } => bytes.len(),
                _ => 0,
            })
            .sum();
        for part in parts {
            to.sharing.receive_requested(from.key, part);
        }
        sent
    }

    #[test]
    fn a_copy_is_not_sent_again_to_a_system_that_already_has_it() {
        let mut a = member(1, None);
        let mut b = member(2, None);
        let mut c = member(3, None);
        let mut d = member(4, Some(text("copied on D")));
        let carried = text("copied on D").text.unwrap().len();
        assert_eq!(hand(&mut d, &mut c, false), carried);
        assert_eq!(hand(&mut c, &mut b, false), carried);
        assert_eq!(hand(&mut b, &mut a, false), carried);
        assert_eq!(a.sharing.clipboard.content, Some(text("copied on D")));
        // diagonally back to C, then on to D: both already have the copy
        let counts = (c.sharing.clipboard.count, d.sharing.clipboard.count);
        assert_eq!(hand(&mut a, &mut c, false), 0, "C already has it");
        assert_eq!(hand(&mut c, &mut d, false), 0, "D is where it was copied");
        assert_eq!(
            (c.sharing.clipboard.count, d.sharing.clipboard.count),
            counts,
            "neither pasteboard is rewritten"
        );
    }

    #[test]
    fn a_new_copy_still_travels() {
        let mut a = member(1, Some(text("old")));
        let mut b = member(2, None);
        hand(&mut a, &mut b, false);
        a.sharing.clipboard.copy(text("new"));
        assert!(hand(&mut a, &mut b, false) > 0);
        assert_eq!(b.sharing.clipboard.content, Some(text("new")));
    }

    #[test]
    fn a_copy_made_on_a_driven_system_reaches_the_next_one() {
        // X drives B; something is copied on B; the pointer moves on to C
        let mut x = member(1, Some(text("old on X")));
        let mut b = member(2, None);
        let mut c = member(3, None);
        b.sharing.clipboard.copy(text("copied on B"));
        // B hands the pointer back through X, offering its copy as it leaves
        assert!(hand(&mut b, &mut x, false) > 0);
        // X drives C and passes the newer copy on
        assert!(hand(&mut x, &mut c, true) > 0);
        assert_eq!(c.sharing.clipboard.content, Some(text("copied on B")));
        // and no copy goes back to B
        assert_eq!(hand(&mut x, &mut b, false), 0);
    }

    #[test]
    fn only_a_requested_copy_is_accepted() {
        let mut x = member(1, Some(text("offered")));
        let mut y = member(2, None);
        let mut z = member(3, Some(text("unsolicited")));
        // an offer from a peer that neither handed control here nor drives this system
        let copy = z.sharing.offer(z.key, y.key).unwrap();
        assert!(!y.sharing.wants(y.key, z.key, copy, None));
        // a snapshot nobody asked for
        let parts = z
            .sharing
            .requested(z.key, y.key, copy, false)
            .map(|read| read())
            .unwrap_or_default();
        for part in parts {
            y.sharing.receive_requested(z.key, part);
        }
        assert_eq!(y.sharing.clipboard.content, None);
        // a peer that was not offered the copy cannot ask for it
        let copy = x.sharing.current(x.key);
        assert!(x.sharing.requested(x.key, y.key, copy, false).is_none());
    }

    #[test]
    fn a_snapshot_for_an_older_request_is_ignored() {
        let mut x = member(1, Some(text("first")));
        let mut y = member(2, None);
        y.sharing.expect_snapshot(Some(x.key));
        let first = x.sharing.offer(x.key, y.key).unwrap();
        assert!(y.sharing.wants(y.key, x.key, first, None));
        let stale = x
            .sharing
            .requested(x.key, y.key, first, false)
            .map(|read| read())
            .unwrap();
        x.sharing.clipboard.copy(text("second"));
        let second = x.sharing.offer(x.key, y.key).unwrap();
        assert!(y.sharing.wants(y.key, x.key, second, Some(x.key)));
        for part in stale {
            y.sharing.receive_requested(x.key, part);
        }
        assert_eq!(
            y.sharing.clipboard.content, None,
            "the older snapshot is not taken for the newer copy"
        );
        for part in x
            .sharing
            .requested(x.key, y.key, second, false)
            .map(|read| read())
            .unwrap()
        {
            y.sharing.receive_requested(x.key, part);
        }
        assert_eq!(y.sharing.clipboard.content, Some(text("second")));
    }
}
