//! The arrangement at the top of Daisy's window: every display of every
//! system in the group, as Displays shows monitors. A peer's displays move
//! together; dragging them places that system.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSLineBreakMode, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSResponder, NSStringDrawing, NSTextAlignment, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::{coral_color, flower_image};
use crate::control::Link;
use crate::identity::PublicKey;
use crate::input::{Rect, Side};
use crate::layout::{Group, Offset};

/// Display sizes are drawn at most this fraction of their size, so one
/// display alone is not drawn huge.
const MAX_SCALE: f64 = 0.1;
const MARGIN: f64 = 6.0;
const INACTIVE_ALPHA: f64 = 0.45;

/// One system as the arrangement shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    pub key: PublicKey,
    pub name: String,
    /// Its displays in its own coordinates, and where they sit in the group.
    pub displays: Vec<Rect>,
    pub offset: Offset,
    pub me: bool,
    /// `None` until a session reports who has control.
    pub in_control: Option<bool>,
    pub locked: bool,
    pub hint: String,
}

impl Shown {
    fn placed(&self) -> impl Iterator<Item = Rect> + '_ {
        self.displays.iter().map(|display| Rect {
            x: display.x + self.offset.0,
            y: display.y + self.offset.1,
            ..*display
        })
    }

    /// The display its name is drawn on: the largest.
    fn main(&self) -> Option<Rect> {
        self.placed()
            .max_by(|a, b| (a.width * a.height).total_cmp(&(b.width * b.height)))
    }
}

/// What the arrangement shows: `layout` while the group runs, or just this
/// system's `displays` when it is alone.
pub fn scene(
    me: PublicKey,
    names: &BTreeMap<PublicKey, String>,
    layout: Option<&Group<PublicKey>>,
    displays: &[Rect],
    links: &BTreeMap<PublicKey, Link>,
) -> Vec<Shown> {
    let alone = Group::alone(me, displays.to_vec());
    let layout = layout
        .filter(|layout| layout.members.iter().any(|member| member.key == me))
        .unwrap_or(&alone);
    let here_in_control = links.values().next().map(|link| link.in_control);
    layout
        .members
        .iter()
        .map(|member| {
            let link = links.get(&member.key);
            let mine = member.key == me;
            let in_control = if mine {
                here_in_control
            } else {
                link.map(|link| link.peer_in_control)
            };
            let locked = link.is_some_and(|link| link.peer_locked);
            let hint = match (mine, in_control, locked) {
                (_, Some(true), _) => "Has control",
                (true, Some(false), _) => "⌃⌥⌘⎋ takes control back",
                (true, None, _) => "",
                (false, _, true) => "Locked; unlock it there",
                (false, _, false) => "Drag to rearrange",
            };
            Shown {
                key: member.key,
                name: names.get(&member.key).cloned().unwrap_or_default(),
                displays: member.displays.clone(),
                offset: member.offset,
                me: mine,
                in_control,
                locked,
                hint: hint.to_owned(),
            }
        })
        .collect()
}

/// How the group's space maps onto a view: `scale` points of view per point
/// of display, after moving by `origin`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fit {
    pub scale: f64,
    pub origin: (f64, f64),
}

impl Fit {
    /// The whole scene, centered in a view of `size`.
    pub fn of(scene: &[Shown], size: (f64, f64)) -> Self {
        let all: Vec<Rect> = scene.iter().flat_map(Shown::placed).collect();
        let Some(bounds) = crate::layout::bounds(&all) else {
            return Self {
                scale: MAX_SCALE,
                origin: (0.0, 0.0),
            };
        };
        let scale = ((size.0 - 2.0 * MARGIN) / bounds.width)
            .min((size.1 - 2.0 * MARGIN) / bounds.height)
            .min(MAX_SCALE);
        let origin = (
            (size.0 - bounds.width * scale) / 2.0 - bounds.x * scale,
            (size.1 - bounds.height * scale) / 2.0 - bounds.y * scale,
        );
        Self { scale, origin }
    }

    pub fn to_view(self, rect: Rect) -> Rect {
        Rect {
            x: rect.x * self.scale + self.origin.0,
            y: rect.y * self.scale + self.origin.1,
            width: rect.width * self.scale,
            height: rect.height * self.scale,
        }
    }

    /// A move of `by` view points, in the group's space.
    pub fn to_group(self, by: (f64, f64)) -> (f64, f64) {
        (by.0 / self.scale, by.1 / self.scale)
    }
}

/// Where `member` sits relative to this system, for people who cannot see
/// the drawing.
pub fn relation(member: &Shown, me: &Shown) -> &'static str {
    let (Some(theirs), Some(mine)) = (
        crate::layout::bounds(&member.placed().collect::<Vec<_>>()),
        crate::layout::bounds(&me.placed().collect::<Vec<_>>()),
    ) else {
        return "beside";
    };
    let dx = (theirs.x + theirs.width / 2.0) - (mine.x + mine.width / 2.0);
    let dy = (theirs.y + theirs.height / 2.0) - (mine.y + mine.height / 2.0);
    if dx.abs() * mine.height >= dy.abs() * mine.width {
        if dx < 0.0 { "left of" } else { "right of" }
    } else if dy < 0.0 {
        "above"
    } else {
        "below"
    }
}

/// The peer after `current` in the scene, for keyboard selection, wrapping
/// around; this system is never selected. `back` goes the other way.
pub fn next_peer(scene: &[Shown], current: Option<PublicKey>, back: bool) -> Option<PublicKey> {
    let peers: Vec<PublicKey> = scene.iter().filter(|shown| !shown.me).map(|shown| shown.key).collect();
    let count = peers.len();
    if count == 0 {
        return None;
    }
    let index = current.and_then(|key| peers.iter().position(|peer| *peer == key));
    let next = match (index, back) {
        (None, false) => 0,
        (None, true) => count - 1,
        (Some(index), false) => (index + 1) % count,
        (Some(index), true) => (index + count - 1) % count,
    };
    Some(peers[next])
}

/// The offset that puts `member` flush against `side` of this system.
pub fn beside(member: &Shown, me: &Shown, side: Side) -> Offset {
    let flush = crate::layout::beside(&me.displays, &member.displays, side);
    (me.offset.0 + flush.0, me.offset.1 + flush.1)
}

/// The member being dragged, where it was grabbed, and how far it moved.
type Drag = (usize, NSPoint, (f64, f64));

pub struct ArrangeIvars {
    checking: Cell<bool>,
    scene: RefCell<Vec<Shown>>,
    drag: Cell<Option<Drag>>,
    /// A member a person just placed, for the action to read.
    placed: Cell<Option<(PublicKey, Offset)>>,
    /// The peer the arrow keys move.
    selected: Cell<Option<PublicKey>>,
    target: RefCell<Weak<AnyObject>>,
    action: Cell<Option<Sel>>,
    /// Tooltip owners; AppKit does not retain them.
    hints: RefCell<Vec<Retained<NSString>>>,
}

define_class!(
    // SAFETY: NSView has no additional subclassing requirements; the view
    // is used only on the main thread.
    #[unsafe(super(NSView, NSResponder, objc2_foundation::NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ArrangeIvars]
    pub struct ArrangeView;

    unsafe impl NSObjectProtocol for ArrangeView {}

    impl ArrangeView {
        // y grows downward, as in the group's space
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            if self.ivars().checking.get() {
                NSColor::secondaryLabelColor().setStroke();
                let size = self.bounds().size;
                let border = NSRect::new(NSPoint::new(1.0, 1.0), NSSize::new(size.width - 2.0, size.height - 2.0));
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(border, 8.0, 8.0).stroke();
            }
            let scene = self.ivars().scene.borrow();
            let fit = self.fit(&scene);
            let drag = self.ivars().drag.get();
            let someone_in_control = scene.iter().any(|shown| shown.in_control == Some(true));
            for (index, shown) in scene.iter().enumerate() {
                let moved = drag
                    .filter(|(dragged, _, _)| *dragged == index)
                    .map_or((0.0, 0.0), |(_, _, by)| by);
                let faded = !self.ivars().checking.get() && someone_in_control && shown.in_control != Some(true);
                let main = shown.main();
                for display in shown.placed() {
                    let mut view = fit.to_view(display);
                    view.x += moved.0;
                    view.y += moved.1;
                    draw_display(shown, view, faded, main == Some(display), scene.len() == 1);
                }
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let scene = self.ivars().scene.borrow();
            let fit = self.fit(&scene);
            let grabbed = scene
                .iter()
                .position(|shown| !shown.me && shown.placed().any(|display| contains(fit.to_view(display), point)));
            drop(scene);
            if let Some(index) = grabbed {
                self.ivars().drag.set(Some((index, point, (0.0, 0.0))));
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some((index, grabbed, _)) = self.ivars().drag.get() else {
                return;
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.ivars()
                .drag
                .set(Some((index, grabbed, (point.x - grabbed.x, point.y - grabbed.y))));
            self.redraw();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let Some((index, _, by)) = self.ivars().drag.take() else {
                return;
            };
            let dropped = {
                let scene = self.ivars().scene.borrow();
                let fit = self.fit(&scene);
                scene.get(index).map(|shown| {
                    let by = fit.to_group(by);
                    (shown.key, (shown.offset.0 + by.0, shown.offset.1 + by.1))
                })
            };
            if let Some((key, offset)) = dropped {
                self.place(key, offset);
            }
        }

        // an open hand over each peer's displays says they can be dragged
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let scene = self.ivars().scene.borrow();
            let fit = self.fit(&scene);
            let hand = objc2_app_kit::NSCursor::openHandCursor();
            for shown in scene.iter().filter(|shown| !shown.me) {
                for display in shown.placed() {
                    self.addCursorRect_cursor(ns_rect(fit.to_view(display)), &hand);
                }
            }
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().scene.borrow().iter().any(|shown| !shown.me)
        }

        // For people not using a pointer: [ and ] choose a peer, and the
        // arrow keys put it against that side of this system.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let key = event.charactersIgnoringModifiers().and_then(|keys| keys.to_string().chars().next());
            if let Some(back) = match key {
                Some('[') => Some(true),
                Some(']') => Some(false),
                _ => None,
            } {
                let scene = self.ivars().scene.borrow();
                let next = next_peer(&scene, self.selected(&scene), back);
                drop(scene);
                self.ivars().selected.set(next);
                self.announce_selection();
                self.setKeyboardFocusRingNeedsDisplayInRect(self.bounds());
                self.redraw();
                return;
            }
            let side = match key {
                Some('\u{F702}') => Some(Side::Left),
                Some('\u{F703}') => Some(Side::Right),
                Some('\u{F700}') => Some(Side::Above),
                Some('\u{F701}') => Some(Side::Below),
                _ => None,
            };
            let placed = side.and_then(|side| {
                let scene = self.ivars().scene.borrow();
                let me = scene.iter().find(|shown| shown.me)?;
                let selected = self.selected(&scene)?;
                let peer = scene.iter().find(|shown| shown.key == selected)?;
                Some((peer.key, beside(peer, me, side)))
            });
            match placed {
                Some((key, offset)) => self.place(key, offset),
                // SAFETY: NSView implements keyDown:
                None => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            self.selected_bounds().map_or(NSRect::ZERO, ns_rect)
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            if let Some(bounds) = self.selected_bounds() {
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(bounds), 6.0, 6.0).fill();
            }
        }
    }
);

impl ArrangeView {
    /// AppKit keeps drawing the focus ring where it last was until told the
    /// mask changed.
    fn redraw(&self) {
        self.setNeedsDisplay(true);
        self.noteFocusRingMaskChanged();
    }

    /// Sends `action` to `target` when a person places a peer's displays.
    pub fn new(mtm: MainThreadMarker, frame: NSRect, target: &AnyObject, action: Sel) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ArrangeIvars {
            checking: Cell::new(false),
            scene: RefCell::new(Vec::new()),
            drag: Cell::new(None),
            placed: Cell::new(None),
            selected: Cell::new(None),
            target: RefCell::new(Weak::from(target)),
            action: Cell::new(Some(action)),
            hints: RefCell::new(Vec::new()),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// Makes `key` the peer the arrow keys move, and takes the keyboard.
    pub fn select(&self, key: Option<PublicKey>) {
        self.ivars().selected.set(key);
        if let Some(window) = self.window() {
            window.makeFirstResponder(Some(self));
        }
        self.announce_selection();
        self.redraw();
    }

    /// The member a person just placed, and where.
    pub fn take_placed(&self) -> Option<(PublicKey, Offset)> {
        self.ivars().placed.take()
    }

    pub fn set_check(&self, checking: bool) {
        if self.ivars().checking.replace(checking) != checking {
            self.setNeedsDisplay(true);
        }
    }

    pub fn show(&self, scene: Vec<Shown>) {
        let me = scene.iter().find(|shown| shown.me).cloned();
        let label = match &me {
            Some(me) if scene.len() > 1 => scene
                .iter()
                .filter(|shown| !shown.me)
                .map(|shown| format!("{} is {} this system", shown.name, relation(shown, me)))
                .collect::<Vec<_>>()
                .join(". "),
            _ => "This system".to_owned(),
        };
        self.setAccessibilityElement(true);
        self.setAccessibilityLabel(Some(&NSString::from_str(&label)));
        if self.ivars().drag.get().is_none() {
            *self.ivars().scene.borrow_mut() = scene;
        }
        self.refresh_hints();
        if let Some(window) = self.window() {
            window.invalidateCursorRectsForView(self);
        }
        self.redraw();
    }

    /// The peer the arrow keys move: the one chosen with [ and ], or else
    /// the first.
    fn selected(&self, scene: &[Shown]) -> Option<PublicKey> {
        self.ivars()
            .selected
            .get()
            .filter(|key| scene.iter().any(|shown| shown.key == *key && !shown.me))
            .or_else(|| next_peer(scene, None, false))
    }

    /// The selected peer's displays, as drawn.
    fn selected_bounds(&self) -> Option<Rect> {
        let scene = self.ivars().scene.borrow();
        let selected = self.selected(&scene)?;
        let index = scene.iter().position(|shown| shown.key == selected)?;
        let fit = self.fit(&scene);
        let moved = self
            .ivars()
            .drag
            .get()
            .filter(|(dragged, _, _)| *dragged == index)
            .map_or((0.0, 0.0), |(_, _, by)| by);
        let bounds = crate::layout::bounds(
            &scene[index]
                .placed()
                .map(|display| fit.to_view(display))
                .collect::<Vec<_>>(),
        )?;
        Some(Rect {
            x: bounds.x + moved.0,
            y: bounds.y + moved.1,
            ..bounds
        })
    }

    /// Tells VoiceOver which peer the arrow keys now move.
    fn announce_selection(&self) {
        let scene = self.ivars().scene.borrow();
        let Some(selected) = self.selected(&scene) else {
            return;
        };
        let Some(shown) = scene.iter().find(|shown| shown.key == selected) else {
            return;
        };
        let me = scene.iter().find(|shown| shown.me);
        let place = me.map_or("", |me| relation(shown, me));
        let text = format!("{}, {place} this system. Arrow keys move it.", shown.name);
        // SAFETY: an NSString is a valid accessibility value
        unsafe { self.setAccessibilityValue(Some(&NSString::from_str(&text))) };
    }

    fn place(&self, key: PublicKey, offset: Offset) {
        self.ivars().placed.set(Some((key, offset)));
        self.redraw();
        let target = self.ivars().target.borrow().load();
        if let (Some(target), Some(action)) = (target, self.ivars().action.get()) {
            // SAFETY: the target implements the action with an object sender
            unsafe {
                NSApplication::sharedApplication(self.mtm()).sendAction_to_from(action, Some(&target), Some(self));
            }
        }
    }

    fn fit(&self, scene: &[Shown]) -> Fit {
        let size = self.bounds().size;
        Fit::of(scene, (size.width, size.height))
    }

    fn refresh_hints(&self) {
        self.removeAllToolTips();
        let mut hints = self.ivars().hints.borrow_mut();
        hints.clear();
        let scene = self.ivars().scene.borrow();
        let fit = self.fit(&scene);
        for shown in scene.iter().filter(|shown| !shown.hint.is_empty()) {
            for display in shown.placed() {
                let hint = NSString::from_str(&shown.hint);
                // SAFETY: the hint is kept alive in the ivars until the
                // tooltips are removed
                unsafe {
                    self.addToolTipRect_owner_userData(ns_rect(fit.to_view(display)), &hint, std::ptr::null_mut())
                };
                hints.push(hint);
            }
        }
    }
}

fn ns_rect(rect: Rect) -> NSRect {
    NSRect::new(NSPoint::new(rect.x, rect.y), NSSize::new(rect.width, rect.height))
}

fn contains(rect: Rect, point: NSPoint) -> bool {
    point.x >= rect.x && point.x <= rect.x + rect.width && point.y >= rect.y && point.y <= rect.y + rect.height
}

/// One display of `shown`: coral with the daisy on the system in control,
/// faded while another system has it. Its name goes on its main display.
fn draw_display(shown: &Shown, view: Rect, faded: bool, main: bool, alone: bool) {
    let alpha = if faded { INACTIVE_ALPHA } else { 1.0 };
    let active = shown.in_control == Some(true);
    let inset = NSRect::new(
        NSPoint::new(view.x + 1.0, view.y + 1.0),
        NSSize::new(view.width - 2.0, view.height - 2.0),
    );
    let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inset, 6.0, 6.0);
    // coral stays the action color, so the system in control is outlined, not filled
    let fill = if active {
        NSColor::controlBackgroundColor()
    } else {
        NSColor::controlBackgroundColor().colorWithAlphaComponent(alpha)
    };
    fill.setFill();
    shape.fill();
    let (stroke, width) = if active {
        (coral_color(), 2.5)
    } else {
        (NSColor::separatorColor().colorWithAlphaComponent(alpha), 1.0)
    };
    stroke.setStroke();
    shape.setLineWidth(width);
    shape.stroke();
    if !main {
        return;
    }

    let with_daisy = active || alone;
    if with_daisy {
        let size = (view.height * 0.34).min(28.0);
        if let Some(daisy) = flower_image(active, size) {
            let at = NSRect::new(
                NSPoint::new(
                    view.x + (view.width - size) / 2.0,
                    view.y + view.height / 2.0 - size - 2.0,
                ),
                NSSize::new(size, size),
            );
            // respectFlipped keeps the daisy upright in this flipped view
            // SAFETY: a zero source rect draws the whole image; no hints
            unsafe {
                daisy.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                    at,
                    NSRect::ZERO,
                    objc2_app_kit::NSCompositingOperation::SourceOver,
                    1.0,
                    true,
                    None,
                )
            };
        }
    }
    let name = if shown.locked {
        format!("{} · Locked", shown.name)
    } else {
        shown.name.clone()
    };
    let name_height = 16.0;
    let name_y = if with_daisy {
        view.y + view.height / 2.0 + 2.0
    } else {
        view.y + (view.height - name_height) / 2.0
    };
    let font = if active {
        NSFont::boldSystemFontOfSize(11.0)
    } else {
        NSFont::systemFontOfSize(11.0)
    };
    let color = NSColor::labelColor().colorWithAlphaComponent(alpha);
    let style = NSMutableParagraphStyle::new();
    style.setAlignment(NSTextAlignment::Center);
    style.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    // SAFETY: AppKit's attribute keys, with values of the types they expect
    let attributes = unsafe {
        NSDictionary::<NSString, AnyObject>::from_slices(
            &[
                NSFontAttributeName,
                NSForegroundColorAttributeName,
                NSParagraphStyleAttributeName,
            ],
            &[font.as_ref(), color.as_ref(), style.as_ref()],
        )
    };
    let name_rect = NSRect::new(
        NSPoint::new(view.x + 4.0, name_y),
        NSSize::new(view.width - 8.0, name_height),
    );
    // SAFETY: the attributes are valid string-drawing attributes
    unsafe { NSString::from_str(&name).drawInRect_withAttributes(name_rect, Some(&attributes)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    const LAPTOP: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1512.0,
        height: 982.0,
    };

    fn names() -> BTreeMap<PublicKey, String> {
        BTreeMap::from([(key(1), "Laptop".to_owned()), (key(2), "Studio".to_owned())])
    }

    fn pair() -> Group<PublicKey> {
        let mut group = Group::alone(key(1), vec![LAPTOP]);
        group.members.push(crate::layout::Member {
            key: key(2),
            displays: vec![LAPTOP],
            offset: (1512.0, 0.0),
        });
        group
    }

    fn link(in_control: bool, peer_in_control: bool, peer_locked: bool) -> Link {
        Link {
            in_control,
            peer_in_control,
            peer_locked,
            ..Link::default()
        }
    }

    #[test]
    fn alone_only_this_system_shows() {
        let shown = scene(key(1), &names(), None, &[LAPTOP], &BTreeMap::new());
        assert_eq!(shown.len(), 1);
        assert!(shown[0].me);
        assert_eq!(shown[0].in_control, None);
    }

    #[test]
    fn every_member_shows_where_the_group_put_it() {
        let links = BTreeMap::from([(key(2), link(true, false, false))]);
        let shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &links);
        assert_eq!(shown.len(), 2);
        let studio = shown.iter().find(|shown| !shown.me).unwrap();
        assert_eq!((studio.name.as_str(), studio.offset), ("Studio", (1512.0, 0.0)));
        assert_eq!(relation(studio, &shown[0]), "right of");
    }

    #[test]
    fn only_the_system_in_control_is_marked_and_hints_stay_short() {
        for (link, mine, theirs) in [
            (link(true, false, false), Some(true), Some(false)),
            (link(false, true, false), Some(false), Some(true)),
            (link(false, false, true), Some(false), Some(false)),
        ] {
            let links = BTreeMap::from([(key(2), link)]);
            let shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &links);
            assert_eq!((shown[0].in_control, shown[1].in_control), (mine, theirs));
            for shown in &shown {
                assert!(shown.hint.split_whitespace().count() <= 5, "{:?}", shown.hint);
            }
        }
        let locked = scene(
            key(1),
            &names(),
            Some(&pair()),
            &[LAPTOP],
            &BTreeMap::from([(key(2), link(true, false, true))]),
        );
        assert!(locked[1].locked);
        assert_eq!(locked[1].hint, "Locked; unlock it there");
    }

    #[test]
    fn the_whole_group_fits_the_view_and_a_drag_is_measured_in_display_points() {
        let shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &BTreeMap::new());
        let fit = Fit::of(&shown, (500.0, 140.0));
        for display in shown.iter().flat_map(Shown::placed) {
            let view = fit.to_view(display);
            assert!(view.x >= 0.0 && view.y >= 0.0, "{view:?}");
            assert!(
                view.x + view.width <= 500.0 && view.y + view.height <= 140.0,
                "{view:?}"
            );
        }
        assert_eq!(fit.to_group((fit.scale * 100.0, -fit.scale * 50.0)), (100.0, -50.0));
        // one display alone is not drawn huge
        let alone = scene(key(1), &names(), None, &[LAPTOP], &BTreeMap::new());
        assert_eq!(Fit::of(&alone, (5000.0, 5000.0)).scale, MAX_SCALE);
    }

    #[test]
    fn brackets_cycle_through_every_peer_and_never_this_system() {
        let mut group = pair();
        group.members.push(crate::layout::Member {
            key: key(3),
            displays: vec![LAPTOP],
            offset: (-1512.0, 0.0),
        });
        let shown = scene(key(1), &names(), Some(&group), &[LAPTOP], &BTreeMap::new());
        assert_eq!(next_peer(&shown, None, false), Some(key(2)));
        assert_eq!(next_peer(&shown, Some(key(2)), false), Some(key(3)));
        assert_eq!(next_peer(&shown, Some(key(3)), false), Some(key(2)));
        assert_eq!(next_peer(&shown, Some(key(2)), true), Some(key(3)));
        assert_eq!(next_peer(&shown, None, true), Some(key(3)));
        let alone = scene(key(1), &names(), None, &[LAPTOP], &BTreeMap::new());
        assert_eq!(next_peer(&alone, None, false), None);
    }

    #[test]
    fn an_arrow_key_puts_the_peer_flush_against_that_side() {
        let shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &BTreeMap::new());
        assert_eq!(beside(&shown[1], &shown[0], Side::Left), (-1512.0, 0.0));
        assert_eq!(beside(&shown[1], &shown[0], Side::Below), (0.0, 982.0));
    }
}
