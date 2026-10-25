//! The arrangement at the top of Daisy's window: every display of every
//! system in the group, as Displays shows monitors. A peer's displays move
//! together; dragging them places that system.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::time::Instant;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSAccessibility, NSAccessibilityGroupRole, NSAccessibilityPostNotification,
    NSAccessibilityValueChangedNotification, NSApplication, NSBezierPath, NSColor, NSEvent, NSFont,
    NSFontAttributeName, NSForegroundColorAttributeName, NSImage, NSLineBreakMode, NSMutableParagraphStyle,
    NSParagraphStyleAttributeName, NSResponder, NSStringDrawing, NSTextAlignment, NSView, NSWindowOcclusionState,
    NSWorkspace,
};
use objc2_foundation::{
    MainThreadMarker, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize,
    NSString, NSTimer,
};

use super::{coral_color, flower_image};
use crate::arrangement_motion::{LINK_SCALE, Motion, Target, chain};
use crate::control::Link;
use crate::identity::PublicKey;
use crate::input::{Rect, Side};
use crate::layout::{Detent, Group, Member, Offset};

/// Display sizes are drawn at most this fraction of their size, so one
/// display alone is not drawn huge.
const MAX_SCALE: f64 = 0.1;
const MARGIN: f64 = 6.0;
const INACTIVE_ALPHA: f64 = 0.45;
/// How close, in view points, a dragged edge comes to a line before it
/// holds there. Display points are too small to aim at in the drawing.
const DETENT: f64 = 6.0;

/// One system as the arrangement shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Shown {
    /// Compact identifier matched to the peer list; this system is 1.
    pub number: usize,
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
                number: if mine {
                    1
                } else {
                    names
                        .keys()
                        .filter(|key| **key != me)
                        .position(|key| *key == member.key)
                        .map_or(0, |index| index + 2)
                },
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

/// The group the scene draws, for placing a member within it.
pub fn group(scene: &[Shown]) -> Group<PublicKey> {
    Group {
        members: scene
            .iter()
            .map(|shown| Member {
                key: shown.key,
                displays: shown.displays.clone(),
                offset: shown.offset,
            })
            .collect(),
    }
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
#[derive(Clone, Copy)]
struct Drag {
    key: PublicKey,
    grabbed: NSPoint,
    origin: Offset,
    by: Offset,
    fit: Fit,
}

pub struct ArrangeIvars {
    checking: Cell<bool>,
    connected_flower: Option<Retained<NSImage>>,
    disconnected_flower: Option<Retained<NSImage>>,
    motion: RefCell<Motion<PublicKey>>,
    timer: RefCell<Option<Retained<NSTimer>>>,
    last_frame: Cell<Option<Instant>>,
    keyboard: Cell<bool>,
    scene: RefCell<Vec<Shown>>,
    drag: Cell<Option<Drag>>,
    detent: RefCell<Detent>,
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
            let scene = self.visual_scene();
            let fit = self.fit(&scene);
            let captions = captions_above(&self.ivars().scene.borrow());
            let motion = self.ivars().motion.borrow();
            let someone_in_control = scene.iter().any(|shown| shown.in_control == Some(true));
            let appearance = |shown: &Shown| {
                motion.bodies.get(&shown.key).map_or(
                    (f64::from(shown.in_control == Some(true)), 1.0, None),
                    |body| (body.emphasis.value.clamp(0.0, 1.0), body.arrival.value.clamp(0.0, 1.04), body.pulse),
                )
            };
            for shown in &scene {
                let (emphasis, arrival, _) = appearance(shown);
                let alpha = if self.ivars().checking.get() || !someone_in_control { 1.0 }
                    else { INACTIVE_ALPHA + (1.0 - INACTIVE_ALPHA) * emphasis };
                for display in shown.placed() {
                    draw_display_fill(arriving(fit.to_view(display), arrival), alpha * arrival);
                }
            }
            for (a, b, from, to) in connections(&scene) {
                let a = &scene[a];
                let b = &scene[b];
                let alpha = motion.bodies.get(&a.key).map_or(1.0, |body| body.connection.value)
                    .min(motion.bodies.get(&b.key).map_or(1.0, |body| body.connection.value)).clamp(0.0, 1.0);
                let from = fit.to_view(from);
                let to = fit.to_view(to);
                let age = if a.in_control == Some(true) { appearance(a).2 } else { appearance(b).2 };
                let (from, to) = if b.in_control == Some(true) { (to, from) } else { (from, to) };
                let above_a = captions.get(&a.key).copied().unwrap_or(false);
                let above_b = captions.get(&b.key).copied().unwrap_or(false);
                // The direction of the pulse follows the system gaining control.
                let (above_from, above_to) = if b.in_control == Some(true) { (above_b, above_a) } else { (above_a, above_b) };
                if compact_display(from) || compact_display(to) {
                    if let Some((start, end)) = compact_connection(from, to) {
                        draw_chain(start, end, 0.0, age, alpha);
                    }
                } else {
                    draw_chain(glyph_center(from, above_from), glyph_center(to, above_to), glyph_size(from).max(glyph_size(to)) / 2.0 + 4.0, age, alpha);
                }
            }
            for shown in &scene {
                let (emphasis, arrival, pulse) = appearance(shown);
                let alpha = if self.ivars().checking.get() || !someone_in_control { 1.0 }
                    else { INACTIVE_ALPHA + (1.0 - INACTIVE_ALPHA) * emphasis };
                let main = shown.main();
                for display in shown.placed() {
                    let flower = if shown.in_control.is_some() { self.ivars().connected_flower.as_deref() } else { self.ivars().disconnected_flower.as_deref() };
                    draw_identity(shown, arriving(fit.to_view(display), arrival), alpha * arrival, main == Some(display), pulse, flower, captions.get(&shown.key).copied().unwrap_or(false));
                }
            }
            // Borders are last: captions and links cannot erase an edge.
            for shown in &scene {
                let (emphasis, arrival, _) = appearance(shown);
                for display in shown.placed() {
                    draw_display_frame(arriving(fit.to_view(display), arrival), emphasis, arrival);
                }
            }
        }

        #[unsafe(method(animateArrangement:))]
        fn animate_arrangement(&self, _timer: &NSTimer) {
            if !self.can_animate() || self.ivars().keyboard.get() {
                self.ivars().motion.borrow_mut().finish();
                self.ivars().drag.set(None);
                self.stop_animation();
                self.refresh_hints();
                self.redraw();
                return;
            }
            let now = Instant::now();
            let elapsed = self.ivars().last_frame.replace(Some(now))
                .map_or(1.0 / 60.0, |last| now.duration_since(last).as_secs_f64());
            let moving = self.ivars().motion.borrow_mut().step(elapsed);
            if self.ivars().drag.get().is_some() && self.window().is_some_and(|window| !window.isKeyWindow()) {
                self.ivars().drag.set(None);
            }
            self.redraw();
            if !moving && self.ivars().drag.get().is_none() {
                self.stop_animation();
                self.refresh_hints();
                if let Some(window) = self.window() { window.invalidateCursorRectsForView(self); }
            }
        }

        #[unsafe(method(viewDidMoveToWindow))]
        fn did_move_to_window(&self) {
            if self.window().is_none() {
                self.ivars().drag.set(None);
                self.stop_animation();
                self.ivars().motion.borrow_mut().finish();
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            self.ivars().keyboard.set(false);
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let scene = self.visual_scene();
            let fit = self.fit(&scene);
            let grabbed = scene.iter().find(|shown| !shown.me && shown.placed().any(|display| contains(fit.to_view(display), point)));
            if let Some(shown) = grabbed {
                self.select(Some(shown.key));
                self.ivars().motion.borrow_mut().frame(fit.scale, fit.origin, false);
                self.ivars().drag.set(Some(Drag { key: shown.key, grabbed: point, origin: shown.offset, by: (0.0, 0.0), fit }));
                self.ivars().detent.replace(Detent::default());
                self.wake();
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some(mut drag) = self.ivars().drag.get() else { return; };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let now = (event.timestamp() * 1000.0) as u64;
            drag.by = self.hold(drag, (point.x - drag.grabbed.x, point.y - drag.grabbed.y), now);
            self.ivars().drag.set(Some(drag));
            self.redraw();
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let Some(drag) = self.ivars().drag.take() else { return; };
            if drag.by == (0.0, 0.0) {
                let size = self.bounds().size;
                let fit = Fit::of(&self.ivars().scene.borrow(), (size.width, size.height));
                self.ivars().motion.borrow_mut().frame(fit.scale, fit.origin, self.can_animate());
                self.wake();
                self.redraw();
                return;
            }
            let by = drag.fit.to_group(drag.by);
            let offset = (drag.origin.0 + by.0, drag.origin.1 + by.1);
            self.ivars().motion.borrow_mut().hold(drag.key, offset);
            self.place(drag.key, offset);
        }

        // an open hand over each peer's displays says they can be dragged
        #[unsafe(method(resetCursorRects))]
        fn reset_cursor_rects(&self) {
            let scene = self.visual_scene();
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
            self.ivars().keyboard.set(true);
            self.ivars().drag.set(None);
            self.ivars().detent.replace(Detent::default());
            self.ivars().motion.borrow_mut().finish();
            self.stop_animation();
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
            self.selected_bounds().map(focus_bounds).map_or(NSRect::ZERO, ns_rect)
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            if let Some(bounds) = self.selected_bounds().map(focus_bounds) {
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(ns_rect(bounds), 3.0, 3.0).fill();
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
            connected_flower: flower_image(true, 32.0),
            disconnected_flower: flower_image(false, 32.0),
            motion: RefCell::new(Motion::default()),
            timer: RefCell::new(None),
            last_frame: Cell::new(None),
            keyboard: Cell::new(false),
            scene: RefCell::new(Vec::new()),
            drag: Cell::new(None),
            detent: RefCell::new(Detent::default()),
            placed: Cell::new(None),
            selected: Cell::new(None),
            target: RefCell::new(Weak::from(target)),
            action: Cell::new(Some(action)),
            hints: RefCell::new(Vec::new()),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// `by`, how far member `index` was dragged in view points, with its
    /// edges held in line where the detent catches them.
    fn hold(&self, drag: Drag, by: Offset, now: u64) -> Offset {
        let scene = self.ivars().scene.borrow();
        let moved = drag.fit.to_group(by);
        let wanted = (drag.origin.0 + moved.0, drag.origin.1 + moved.1);
        let held =
            self.ivars()
                .detent
                .borrow_mut()
                .follow(&group(&scene), drag.key, wanted, DETENT / drag.fit.scale, now);
        (
            (held.0 - drag.origin.0) * drag.fit.scale,
            (held.1 - drag.origin.1) * drag.fit.scale,
        )
    }

    fn can_animate(&self) -> bool {
        !NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
            && !self.isHiddenOrHasHiddenAncestor()
            && self.window().is_some_and(|window| {
                window.isVisible()
                    && !window.isMiniaturized()
                    && window.occlusionState().contains(NSWindowOcclusionState::Visible)
            })
    }

    fn stop_animation(&self) {
        if let Some(timer) = self.ivars().timer.borrow_mut().take() {
            timer.invalidate();
        }
        self.ivars().last_frame.set(None);
    }

    fn wake(&self) {
        if !self.can_animate() || self.ivars().keyboard.get() {
            self.ivars().motion.borrow_mut().finish();
            self.stop_animation();
            return;
        }
        if self.ivars().timer.borrow().is_some() {
            return;
        }
        self.ivars().last_frame.set(Some(Instant::now()));
        // SAFETY: the selector is implemented above, and the timer is attached
        // only to this main-thread run loop. It invalidates when settled or hidden.
        let timer = unsafe {
            let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                1.0 / 60.0,
                self,
                sel!(animateArrangement:),
                None,
                true,
            );
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            timer
        };
        *self.ivars().timer.borrow_mut() = Some(timer);
    }

    fn visual_scene(&self) -> Vec<Shown> {
        let mut scene = self.ivars().scene.borrow().clone();
        let motion = self.ivars().motion.borrow();
        for shown in &mut scene {
            if let Some(body) = motion.bodies.get(&shown.key) {
                shown.offset = (body.x.value, body.y.value);
            }
            if let Some(drag) = self.ivars().drag.get().filter(|drag| drag.key == shown.key) {
                let by = drag.fit.to_group(drag.by);
                shown.offset = (drag.origin.0 + by.0, drag.origin.1 + by.1);
            }
        }
        scene
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
        if *self.ivars().scene.borrow() == scene {
            let motion = self.ivars().motion.borrow();
            if scene.iter().all(|shown| {
                motion
                    .bodies
                    .get(&shown.key)
                    .is_some_and(|body| (body.x.goal, body.y.goal) == shown.offset)
            }) {
                return;
            }
        }
        let me = scene.iter().find(|shown| shown.me).cloned();
        let label = match &me {
            Some(me) if scene.len() > 1 => scene
                .iter()
                .filter(|shown| !shown.me)
                .map(|shown| {
                    format!(
                        "{} is {} this system{}",
                        shown.name,
                        relation(shown, me),
                        if shown.in_control == Some(true) {
                            "; has control"
                        } else if shown.locked {
                            "; locked"
                        } else {
                            ""
                        }
                    )
                })
                .collect::<Vec<_>>()
                .join(". "),
            _ => "This system".to_owned(),
        };
        self.setAccessibilityElement(true);
        // SAFETY: this is AppKit's constant group role.
        self.setAccessibilityRole(Some(unsafe { NSAccessibilityGroupRole }));
        let local = if me.as_ref().is_some_and(|shown| shown.in_control == Some(true)) {
            "This system has control. "
        } else {
            ""
        };
        self.setAccessibilityLabel(Some(&NSString::from_str(&format!("{local}{label}"))));
        self.setAccessibilityHelp(Some(&NSString::from_str(
            "Screen arrangement. Use [ and ] to choose a peer, then arrow keys to place it beside this system.",
        )));
        if self.ivars().drag.get().is_some_and(|drag| {
            let old = self.ivars().scene.borrow();
            let previous = old.iter().find(|shown| shown.key == drag.key);
            let next = scene.iter().find(|shown| shown.key == drag.key);
            next.is_none() || previous.map(|shown| &shown.displays) != next.map(|shown| &shown.displays)
        }) {
            self.ivars().drag.set(None);
        }
        let animate = self.can_animate() && !self.ivars().keyboard.get();
        let targets: Vec<_> = scene
            .iter()
            .map(|shown| Target {
                key: shown.key,
                offset: shown.offset,
                active: shown.in_control == Some(true),
                connected: shown.in_control.is_some(),
            })
            .collect();
        let size = self.bounds().size;
        let fit = Fit::of(&scene, (size.width, size.height));
        {
            let mut motion = self.ivars().motion.borrow_mut();
            motion.sync(&targets, animate);
            if self.ivars().drag.get().is_none() {
                motion.frame(fit.scale, fit.origin, animate);
            }
        }
        *self.ivars().scene.borrow_mut() = scene;
        self.announce_selection();
        self.wake();
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
        let scene = self.visual_scene();
        let selected = self.selected(&scene)?;
        let shown = scene.iter().find(|shown| shown.key == selected)?;
        let fit = self.fit(&scene);
        crate::layout::bounds(&shown.placed().map(|display| fit.to_view(display)).collect::<Vec<_>>())
    }

    /// Tells VoiceOver which peer the arrow keys now move.
    fn announce_selection(&self) {
        let text = {
            let scene = self.ivars().scene.borrow();
            self.selected(&scene)
                .and_then(|selected| scene.iter().find(|shown| shown.key == selected))
                .map(|shown| {
                    let me = scene.iter().find(|shown| shown.me);
                    let place = me.map_or("", |me| relation(shown, me));
                    format!("{}, {place} this system. Arrow keys move it.", shown.name)
                })
        };
        let previous = self.accessibilityValue();
        let unchanged = match &text {
            Some(text) => previous
                .as_ref()
                .and_then(|value| value.downcast_ref::<NSString>())
                .is_some_and(|value| value.to_string() == *text),
            None => previous.is_none(),
        };
        if unchanged {
            return;
        }
        let text = text.as_deref().map(NSString::from_str);
        // SAFETY: the NSString is an accessibility value, and this NSView lives
        // on AppKit's main thread. Notify clients only when the value changes.
        unsafe {
            self.setAccessibilityValue(text.as_ref().map(|text| text.as_ref() as &AnyObject));
            NSAccessibilityPostNotification(self, NSAccessibilityValueChangedNotification);
        }
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
        if let Some(drag) = self.ivars().drag.get() {
            return drag.fit;
        }
        if let Some(frame) = self.ivars().motion.borrow().frame {
            return Fit {
                scale: frame[0].value.clamp(0.000001, MAX_SCALE),
                origin: (frame[1].value, frame[2].value),
            };
        }
        let size = self.bounds().size;
        Fit::of(scene, (size.width, size.height))
    }

    fn refresh_hints(&self) {
        self.removeAllToolTips();
        let mut hints = self.ivars().hints.borrow_mut();
        hints.clear();
        let scene = self.visual_scene();
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

/// Direct screen adjacency uses the same geometry as pointer reachability.
/// Only live systems gain links; a remembered offline peer never looks connected.
fn connections(scene: &[Shown]) -> Vec<(usize, usize, Rect, Rect)> {
    let displays: Vec<_> = scene
        .iter()
        .enumerate()
        .flat_map(|(index, shown)| {
            shown.placed().map(move |rect| {
                (
                    index,
                    crate::arrangement_motion::Display {
                        rect,
                        connected: shown.in_control.is_some(),
                    },
                )
            })
        })
        .collect();
    let geometry: Vec<_> = displays.iter().map(|(_, display)| *display).collect();
    crate::arrangement_motion::connections(&geometry)
        .into_iter()
        .map(|(a, b)| (displays[a].0, displays[b].0, displays[a].1.rect, displays[b].1.rect))
        .collect()
}

fn captions_above(scene: &[Shown]) -> BTreeMap<PublicKey, bool> {
    let mut above = BTreeMap::new();
    for (a, b, from, to) in connections(scene) {
        if crate::arrangement_motion::caption_above(from, to) {
            above.insert(scene[a].key, true);
        }
        if crate::arrangement_motion::caption_above(to, from) {
            above.insert(scene[b].key, true);
        }
    }
    above
}

fn arriving(rect: Rect, arrival: f64) -> Rect {
    let scale = 0.88 + 0.12 * arrival;
    Rect {
        x: rect.x + rect.width * (1.0 - scale) / 2.0,
        y: rect.y + rect.height * (1.0 - scale) / 2.0,
        width: rect.width * scale,
        height: rect.height * scale,
    }
}

fn compact_display(view: Rect) -> bool {
    view.width < 140.0 || view.height < 64.0
}

/// Dense arrangements use a short pair of links at the shared edge, leaving
/// identifiers clear. A corner contact needs no diagonal across other labels.
fn compact_connection(from: Rect, to: Rect) -> Option<(Offset, Offset)> {
    let overlap_x = (from.x + from.width).min(to.x + to.width) - from.x.max(to.x);
    let overlap_y = (from.y + from.height).min(to.y + to.height) - from.y.max(to.y);
    if overlap_y > 1.0 && overlap_x <= 1.0 {
        let y = from.y.max(to.y) + overlap_y / 2.0;
        let x = if from.x < to.x {
            (from.x + from.width + to.x) / 2.0
        } else {
            (to.x + to.width + from.x) / 2.0
        };
        let sign = if from.x < to.x { 1.0 } else { -1.0 };
        Some(((x - 16.0 * sign, y), (x + 16.0 * sign, y)))
    } else if overlap_x > 1.0 && overlap_y <= 1.0 {
        let x = from.x.max(to.x) + overlap_x / 2.0;
        let y = if from.y < to.y {
            (from.y + from.height + to.y) / 2.0
        } else {
            (to.y + to.height + from.y) / 2.0
        };
        let sign = if from.y < to.y { 1.0 } else { -1.0 };
        Some(((x, y - 16.0 * sign), (x, y + 16.0 * sign)))
    } else {
        None
    }
}

fn glyph_size(view: Rect) -> f64 {
    (view.height * 0.26).min(28.0)
}

fn glyph_center(view: Rect, caption_above: bool) -> Offset {
    (
        view.x + view.width / 2.0,
        view.y + view.height / 2.0 + (glyph_size(view) / 2.0 + 2.0) * if caption_above { 1.0 } else { -1.0 },
    )
}

fn draw_chain(from: Offset, to: Offset, clearance: f64, pulse: Option<f64>, alpha: f64) {
    NSColor::colorWithSRGBRed_green_blue_alpha(0.45, 0.49, 0.52, alpha).setStroke();
    for link in chain(from, to, clearance, pulse) {
        let (sin, cos) = link.angle.sin_cos();
        let point = |x: f64, y: f64| {
            NSPoint::new(
                link.center.0 + (x * cos - y * sin) * LINK_SCALE,
                link.center.1 + (x * sin + y * cos) * LINK_SCALE,
            )
        };
        let path = NSBezierPath::bezierPath();
        path.moveToPoint(point(21.5, -13.0));
        path.lineToPoint(point(-8.5, -13.0));
        path.curveToPoint_controlPoint1_controlPoint2(point(-21.5, 0.0), point(-15.68, -13.0), point(-21.5, -7.18));
        path.curveToPoint_controlPoint1_controlPoint2(point(-8.5, 13.0), point(-21.5, 7.18), point(-15.68, 13.0));
        path.lineToPoint(point(21.5, 13.0));
        path.setLineWidth(9.5 * LINK_SCALE);
        path.stroke();
    }
}

fn focus_bounds(view: Rect) -> Rect {
    let inset = 6.0_f64.min(view.width / 4.0).min(view.height / 4.0);
    Rect {
        x: view.x + inset,
        y: view.y + inset,
        width: (view.width - inset * 2.0).max(0.0),
        height: (view.height - inset * 2.0).max(0.0),
    }
}

fn display_path(view: Rect, inset: f64) -> Retained<NSBezierPath> {
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
        NSRect::new(
            NSPoint::new(view.x + inset, view.y + inset),
            NSSize::new(
                (view.width - 2.0 * inset).max(0.0),
                (view.height - 2.0 * inset).max(0.0),
            ),
        ),
        6.0,
        6.0,
    )
}

fn draw_display_fill(view: Rect, alpha: f64) {
    NSColor::controlBackgroundColor()
        .colorWithAlphaComponent(alpha.clamp(0.0, 1.0))
        .setFill();
    display_path(view, 1.0).fill();
}

fn draw_display_frame(view: Rect, emphasis: f64, alpha: f64) {
    let shape = display_path(view, 1.0);
    NSColor::separatorColor()
        .colorWithAlphaComponent(alpha.clamp(0.0, 1.0))
        .setStroke();
    shape.setLineWidth(1.0);
    shape.stroke();
    if emphasis > 0.001 {
        coral_color()
            .colorWithAlphaComponent(emphasis * alpha.clamp(0.0, 1.0))
            .setStroke();
        let accent = display_path(view, 3.0);
        accent.setLineWidth(1.5);
        accent.stroke();
    }
}

fn draw_identity(
    shown: &Shown,
    view: Rect,
    alpha: f64,
    main: bool,
    pulse: Option<f64>,
    flower: Option<&NSImage>,
    caption_above: bool,
) {
    let active = shown.in_control == Some(true);
    let size = glyph_size(view);
    let bloom = pulse.map_or(1.0, |age| {
        1.0 + ((age / 0.55).clamp(0.0, 1.0) * std::f64::consts::PI).sin() * 0.07
    });
    if let Some(daisy) = flower.filter(|_| !compact_display(view)) {
        let center = glyph_center(view, caption_above);
        let at = NSRect::new(
            NSPoint::new(center.0 - size * bloom / 2.0, center.1 - size * bloom / 2.0),
            NSSize::new(size * bloom, size * bloom),
        );
        // SAFETY: a zero source rect draws the whole image; no hints. Alpha
        // and scale affect only the presentation, never the system's state.
        unsafe {
            daisy.drawInRect_fromRect_operation_fraction_respectFlipped_hints(
                at,
                NSRect::ZERO,
                objc2_app_kit::NSCompositingOperation::SourceOver,
                alpha.clamp(0.0, 1.0),
                true,
                None,
            )
        };
    }
    if !main {
        return;
    }
    let compact = compact_display(view);
    let name = if compact {
        if shown.me && view.width >= 110.0 {
            format!("{} · This system", shown.number)
        } else if shown.me && view.width >= 60.0 {
            format!("{} · Here", shown.number)
        } else {
            shown.number.to_string()
        }
    } else if shown.locked {
        format!("{} · {} · Locked", shown.number, shown.name)
    } else {
        if active {
            format!("{} · {} · In use", shown.number, shown.name)
        } else {
            format!("{} · {}", shown.number, shown.name)
        }
    };
    let name_height = 16.0;
    let name_y = if compact {
        view.y + (view.height - name_height) / 2.0
    } else {
        (view.y + view.height / 2.0 + if caption_above { -18.0 } else { 2.0 })
            .clamp(view.y + 5.0, view.y + view.height - name_height - 5.0)
    };
    let font = if active {
        NSFont::boldSystemFontOfSize(11.0)
    } else {
        NSFont::systemFontOfSize(11.0)
    };
    let color = NSColor::labelColor();
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
    let name = NSString::from_str(&name);
    // Labels sit above the chain, including vertical and corner connections.
    // Keep a small opaque backing so a link can never cut through a name.
    let width = unsafe { name.sizeWithAttributes(Some(&attributes)) }
        .width
        .min(name_rect.size.width);
    NSColor::controlBackgroundColor().setFill();
    NSBezierPath::bezierPathWithRect(NSRect::new(
        NSPoint::new(view.x + (view.width - width) / 2.0 - 2.0, name_y),
        NSSize::new(width + 4.0, name_height),
    ))
    .fill();
    // SAFETY: the attributes are valid string-drawing attributes.
    unsafe { name.drawInRect_withAttributes(name_rect, Some(&attributes)) };
}

#[cfg(test)]
#[path = "map_test.rs"]
mod native_tests;

#[cfg(test)]
pub(super) fn native_presentation(mtm: MainThreadMarker, target: &AnyObject) {
    native_tests::run(mtm, target);
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
    fn chain_edges_require_live_sessions_and_pointer_reachability() {
        let links = BTreeMap::from([(key(2), link(true, false, false))]);
        let mut shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &links);
        assert_eq!(connections(&shown).len(), 1);
        shown[1].offset = (5000.0, 0.0);
        assert!(
            connections(&shown).is_empty(),
            "a distant screen never gains a decorative link"
        );
        shown[1].offset = (1512.0, 0.0);
        shown[1].in_control = None;
        assert!(
            connections(&shown).is_empty(),
            "remembered offline screens are not connected"
        );
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
    fn the_scene_places_members_in_the_group_it_was_drawn_from() {
        let shown = scene(key(1), &names(), Some(&pair()), &[LAPTOP], &BTreeMap::new());
        assert_eq!(group(&shown), pair());
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
