//! The arrangement at the top of Daisy's window: this system's screen and,
//! while connected, the peer's beside it. Like Displays → Arrange, the
//! peer's screen can be dragged to another side.

use std::cell::{Cell, RefCell};
use std::time::Duration;

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, Sel};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSAccessibility, NSApplication, NSBezierPath, NSColor, NSEvent, NSFont, NSFontAttributeName,
    NSForegroundColorAttributeName, NSLineBreakMode, NSMutableParagraphStyle, NSParagraphStyleAttributeName,
    NSResponder, NSStringDrawing, NSTextAlignment, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};

use super::{coral_color, flower_image, span};
use crate::control::Link;
use crate::input::Side;

/// A screen tile's size before scaling to fit, about a 16:10 display.
const TILE: (f64, f64) = (148.0, 94.0);
/// Space between the two tiles.
const GAP: f64 = 4.0;
const INACTIVE_ALPHA: f64 = 0.45;

/// What the arrangement shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arrangement {
    pub here: Tile,
    /// The peer, and the side of this screen it sits on, while connected.
    pub there: Option<(Tile, Side)>,
    pub link: String,
    pub link_hint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tile {
    pub name: String,
    /// `None` until a session reports who has control.
    pub in_control: Option<bool>,
    pub hint: String,
}

/// The running session's latest report, and how long it has run.
pub type Live = Option<(Link, Duration)>;

/// `connected` is the peer, where it sits, and how the session is going.
pub fn arrangement(local: &str, connected: Option<(&str, Side, Live)>) -> Arrangement {
    let Some((peer, side, live)) = connected else {
        return Arrangement {
            here: Tile {
                name: local.to_owned(),
                in_control: None,
                hint: String::new(),
            },
            there: None,
            link: String::new(),
            link_hint: String::new(),
        };
    };
    let here_in_control = live.map(|(link, _)| link.in_control);
    let here = Tile {
        name: local.to_owned(),
        in_control: here_in_control,
        hint: match here_in_control {
            Some(true) => "Has control".to_owned(),
            Some(false) => "⌃⌥⌘⎋ takes control back".to_owned(),
            None => String::new(),
        },
    };
    let there = Tile {
        name: peer.to_owned(),
        in_control: here_in_control.map(|here| !here),
        hint: match here_in_control {
            Some(false) => "Has control. Drag to rearrange.".to_owned(),
            _ => "Drag to rearrange".to_owned(),
        },
    };
    let (link, link_hint) = match live {
        Some((link, connected_for)) => (
            link.latency_ms.map(|ms| format!("{ms} ms")).unwrap_or_default(),
            format!("Connected for {}", span(connected_for)),
        ),
        None => (String::new(), String::new()),
    };
    Arrangement {
        here,
        there: Some((there, side)),
        link,
        link_hint,
    }
}

/// `(x, y, width, height)`, with y growing upward as in AppKit.
pub type Frame = (f64, f64, f64, f64);

/// Where the tiles go in a view of `size`: this system's screen, and the
/// peer's on `side` of it. The pair is centered and scaled to fit.
pub fn frames(size: (f64, f64), side: Option<Side>) -> (Frame, Option<Frame>) {
    let (columns, rows) = match side {
        None => (1.0, 1.0),
        Some(Side::Left | Side::Right) => (2.0, 1.0),
        Some(Side::Above | Side::Below) => (1.0, 2.0),
    };
    let extent = (
        TILE.0 * columns + GAP * (columns - 1.0),
        TILE.1 * rows + GAP * (rows - 1.0),
    );
    let scale = ((size.0 - 8.0) / extent.0).min((size.1 - 8.0) / extent.1).min(1.0);
    let (width, height) = (TILE.0 * scale, TILE.1 * scale);
    let gap = GAP * scale;
    let origin = ((size.0 - extent.0 * scale) / 2.0, (size.1 - extent.1 * scale) / 2.0);
    let (here, there) = match side {
        None => ((origin.0, origin.1), None),
        Some(Side::Right) => ((origin.0, origin.1), Some((origin.0 + width + gap, origin.1))),
        Some(Side::Left) => ((origin.0 + width + gap, origin.1), Some((origin.0, origin.1))),
        Some(Side::Above) => ((origin.0, origin.1), Some((origin.0, origin.1 + height + gap))),
        Some(Side::Below) => ((origin.0, origin.1 + height + gap), Some((origin.0, origin.1))),
    };
    (
        (here.0, here.1, width, height),
        there.map(|there| (there.0, there.1, width, height)),
    )
}

/// The side a peer's screen dropped `offset` from this one's center (y
/// upward) snaps to. Width and height weigh the distances, so a drop on
/// the diagonal of a wide screen goes to the nearer edge.
pub fn side_at(offset: (f64, f64)) -> Side {
    if offset.0.abs() * TILE.1 >= offset.1.abs() * TILE.0 {
        if offset.0 < 0.0 { Side::Left } else { Side::Right }
    } else if offset.1 > 0.0 {
        Side::Above
    } else {
        Side::Below
    }
}

pub struct ArrangeIvars {
    shown: RefCell<Option<Arrangement>>,
    /// Where the peer's tile is while dragged, and where it was grabbed.
    drag: Cell<Option<(NSPoint, NSPoint)>>,
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
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: NSRect) {
            let Some(shown) = self.ivars().shown.borrow().clone() else {
                return;
            };
            let (here, there) = self.tile_rects(&shown);
            draw_tile(&shown.here, here, shown.there.is_none());
            if let (Some((tile, _)), Some(rect)) = (&shown.there, there) {
                draw_tile(tile, rect, false);
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            let shown = self.ivars().shown.borrow().clone();
            if let Some(shown) = shown
                && let (_, Some(there)) = self.tile_rects(&shown)
                && contains(there, point)
            {
                self.ivars().drag.set(Some((there.origin, NSPoint::new(point.x - there.origin.x, point.y - there.origin.y))));
            }
        }

        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, event: &NSEvent) {
            let Some((_, grab)) = self.ivars().drag.get() else {
                return;
            };
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            self.ivars().drag.set(Some((NSPoint::new(point.x - grab.x, point.y - grab.y), grab)));
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, _event: &NSEvent) {
            let Some((dropped, _)) = self.ivars().drag.take() else {
                return;
            };
            let shown = self.ivars().shown.borrow().clone();
            if let Some(shown) = shown {
                let size = self.bounds().size;
                let (here, _) = frames((size.width, size.height), shown.there.as_ref().map(|(_, side)| *side));
                let offset = (
                    dropped.x + here.2 / 2.0 - (here.0 + here.2 / 2.0),
                    dropped.y + here.3 / 2.0 - (here.1 + here.3 / 2.0),
                );
                self.choose(side_at(offset));
            }
            self.setNeedsDisplay(true);
        }

        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            self.ivars().shown.borrow().as_ref().is_some_and(|shown| shown.there.is_some())
        }

        // Arrow keys move the peer's screen, for people not using a pointer.
        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            let side = event.charactersIgnoringModifiers().and_then(|keys| match keys.to_string().chars().next() {
                Some('\u{F702}') => Some(Side::Left),
                Some('\u{F703}') => Some(Side::Right),
                Some('\u{F700}') => Some(Side::Above),
                Some('\u{F701}') => Some(Side::Below),
                _ => None,
            });
            match side {
                Some(side) => self.choose(side),
                // SAFETY: NSView implements keyDown:
                None => unsafe { msg_send![super(self), keyDown: event] },
            }
        }

        #[unsafe(method(focusRingMaskBounds))]
        fn focus_ring_mask_bounds(&self) -> NSRect {
            self.peer_rect()
        }

        #[unsafe(method(drawFocusRingMask))]
        fn draw_focus_ring_mask(&self) {
            NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(self.peer_rect(), 8.0, 8.0).fill();
        }
    }
);

impl ArrangeView {
    /// Sends `action` to `target` when the peer's screen is moved.
    pub fn new(mtm: MainThreadMarker, frame: NSRect, target: &AnyObject, action: Sel) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ArrangeIvars {
            shown: RefCell::new(None),
            drag: Cell::new(None),
            target: RefCell::new(Weak::from(target)),
            action: Cell::new(Some(action)),
            hints: RefCell::new(Vec::new()),
        });
        // SAFETY: initWithFrame: is NSView's designated initializer
        unsafe { msg_send![super(this), initWithFrame: frame] }
    }

    /// The side the peer's screen was last moved to.
    pub fn side(&self) -> Option<Side> {
        self.ivars()
            .shown
            .borrow()
            .as_ref()?
            .there
            .as_ref()
            .map(|(_, side)| *side)
    }

    pub fn show(&self, arrangement: Arrangement) {
        let label = match &arrangement.there {
            Some((there, side)) => format!("{} is {} {}", there.name, place(*side), arrangement.here.name),
            None => arrangement.here.name.clone(),
        };
        self.setAccessibilityElement(true);
        self.setAccessibilityLabel(Some(&NSString::from_str(&label)));
        *self.ivars().shown.borrow_mut() = Some(arrangement);
        self.refresh_hints();
        self.setNeedsDisplay(true);
    }

    fn choose(&self, side: Side) {
        let changed = {
            let mut shown = self.ivars().shown.borrow_mut();
            match shown.as_mut().and_then(|shown| shown.there.as_mut()) {
                Some((_, current)) if *current != side => {
                    *current = side;
                    true
                }
                _ => false,
            }
        };
        self.refresh_hints();
        self.setNeedsDisplay(true);
        let target = self.ivars().target.borrow().load();
        if changed && let (Some(target), Some(action)) = (target, self.ivars().action.get()) {
            // SAFETY: the target implements the action with an object sender
            unsafe {
                NSApplication::sharedApplication(self.mtm()).sendAction_to_from(action, Some(&target), Some(self));
            }
        }
    }

    fn peer_rect(&self) -> NSRect {
        let shown = self.ivars().shown.borrow().clone();
        shown
            .and_then(|shown| self.tile_rects(&shown).1)
            .unwrap_or(NSRect::ZERO)
    }

    fn tile_rects(&self, shown: &Arrangement) -> (NSRect, Option<NSRect>) {
        let size = self.bounds().size;
        let (here, there) = frames((size.width, size.height), shown.there.as_ref().map(|(_, side)| *side));
        let there = there.map(|there| match self.ivars().drag.get() {
            Some((origin, _)) => rect((origin.x, origin.y, there.2, there.3)),
            None => rect(there),
        });
        (rect(here), there)
    }

    fn refresh_hints(&self) {
        self.removeAllToolTips();
        let mut hints = self.ivars().hints.borrow_mut();
        hints.clear();
        let Some(shown) = self.ivars().shown.borrow().clone() else {
            return;
        };
        let (here, there) = self.tile_rects(&shown);
        let tiles = [
            (Some(&shown.here), Some(here)),
            (shown.there.as_ref().map(|(tile, _)| tile), there),
        ];
        for (tile, rect) in tiles {
            if let (Some(tile), Some(rect)) = (tile, rect)
                && !tile.hint.is_empty()
            {
                let hint = NSString::from_str(&tile.hint);
                // SAFETY: the hint is kept alive in the ivars until the
                // tooltips are removed
                unsafe { self.addToolTipRect_owner_userData(rect, &hint, std::ptr::null_mut()) };
                hints.push(hint);
            }
        }
    }
}

fn place(side: Side) -> &'static str {
    match side {
        Side::Left => "left of",
        Side::Right => "right of",
        Side::Above => "above",
        Side::Below => "below",
    }
}

fn rect(frame: Frame) -> NSRect {
    NSRect::new(NSPoint::new(frame.0, frame.1), NSSize::new(frame.2, frame.3))
}

fn contains(rect: NSRect, point: NSPoint) -> bool {
    point.x >= rect.origin.x
        && point.x <= rect.origin.x + rect.size.width
        && point.y >= rect.origin.y
        && point.y <= rect.origin.y + rect.size.height
}

/// One screen: coral with the daisy while it has control, faded while the
/// other one does. `alone` is this system's screen with nothing connected.
fn draw_tile(tile: &Tile, rect: NSRect, alone: bool) {
    let alpha = if tile.in_control == Some(false) {
        INACTIVE_ALPHA
    } else {
        1.0
    };
    let active = tile.in_control == Some(true);
    let inset = NSRect::new(
        NSPoint::new(rect.origin.x + 1.0, rect.origin.y + 1.0),
        NSSize::new(rect.size.width - 2.0, rect.size.height - 2.0),
    );
    let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(inset, 8.0, 8.0);
    let fill = if active {
        coral_color().colorWithAlphaComponent(0.14)
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

    if active || alone {
        let size = (rect.size.height * 0.34).min(30.0);
        if let Some(daisy) = flower_image(active, size) {
            let at = NSRect::new(
                NSPoint::new(
                    rect.origin.x + (rect.size.width - size) / 2.0,
                    rect.origin.y + rect.size.height / 2.0 + 2.0,
                ),
                NSSize::new(size, size),
            );
            daisy.drawInRect(at);
        }
    }
    let name_height = 18.0;
    let name_y = if active || alone {
        rect.origin.y + rect.size.height / 2.0 - name_height - 2.0
    } else {
        rect.origin.y + (rect.size.height - name_height) / 2.0
    };
    let font = if active {
        NSFont::boldSystemFontOfSize(12.0)
    } else {
        NSFont::systemFontOfSize(12.0)
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
        NSPoint::new(rect.origin.x + 6.0, name_y),
        NSSize::new(rect.size.width - 12.0, name_height),
    );
    // SAFETY: the attributes are valid string-drawing attributes
    unsafe { NSString::from_str(&tile.name).drawInRect_withAttributes(name_rect, Some(&attributes)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live(in_control: bool, latency_ms: Option<u64>) -> Live {
        Some((Link { latency_ms, in_control }, Duration::from_secs(35 * 60)))
    }

    #[test]
    fn only_this_screen_shows_while_not_connected() {
        let shown = arrangement("Laptop", None);
        assert_eq!(shown.here.name, "Laptop");
        assert_eq!(shown.there, None);
        let (here, there) = frames((500.0, 140.0), None);
        assert_eq!(there, None);
        assert!((here.0 + here.2 / 2.0 - 250.0).abs() < 0.01, "centered: {here:?}");
    }

    #[test]
    fn the_peer_sits_on_its_side() {
        let (here, there) = frames((500.0, 140.0), Some(Side::Right));
        assert!(there.unwrap().0 > here.0);
        let (here, there) = frames((500.0, 140.0), Some(Side::Left));
        assert!(there.unwrap().0 < here.0);
        let (here, there) = frames((500.0, 140.0), Some(Side::Above));
        assert!(there.unwrap().1 > here.1);
        let (here, there) = frames((500.0, 140.0), Some(Side::Below));
        assert!(there.unwrap().1 < here.1);
    }

    #[test]
    fn stacked_screens_shrink_to_fit() {
        let (here, there) = frames((500.0, 140.0), Some(Side::Above));
        let there = there.unwrap();
        assert!(here.1 >= 0.0 && there.1 + there.3 <= 140.0, "{here:?} {there:?}");
    }

    #[test]
    fn a_drop_snaps_to_the_nearest_side() {
        assert_eq!(side_at((200.0, 10.0)), Side::Right);
        assert_eq!(side_at((-200.0, -10.0)), Side::Left);
        assert_eq!(side_at((10.0, 120.0)), Side::Above);
        assert_eq!(side_at((-10.0, -120.0)), Side::Below);
        // on the diagonal of a wide screen, the side wins
        assert_eq!(side_at((148.0, 94.0)), Side::Right);
        assert_eq!(side_at((100.0, 94.0)), Side::Above);
    }

    #[test]
    fn only_the_system_in_control_is_marked() {
        let shown = arrangement("Laptop", Some(("Studio", Side::Right, live(true, Some(12)))));
        let (there, _) = shown.there.clone().unwrap();
        assert_eq!((shown.here.in_control, there.in_control), (Some(true), Some(false)));
        assert_eq!(shown.here.hint, "Has control");
        assert_eq!(there.hint, "Drag to rearrange");

        let shown = arrangement("Laptop", Some(("Studio", Side::Right, live(false, Some(12)))));
        let (there, _) = shown.there.clone().unwrap();
        assert_eq!((shown.here.in_control, there.in_control), (Some(false), Some(true)));
        assert_eq!(shown.here.hint, "⌃⌥⌘⎋ takes control back");

        let shown = arrangement("Laptop", Some(("Studio", Side::Right, None)));
        assert_eq!(shown.here.in_control, None);
    }

    #[test]
    fn the_link_shows_latency_and_hints_duration() {
        let shown = arrangement("Laptop", Some(("Studio", Side::Right, live(true, Some(12)))));
        assert_eq!(shown.link, "12 ms");
        assert_eq!(shown.link_hint, "Connected for 35 min");
        let shown = arrangement("Laptop", Some(("Studio", Side::Right, live(true, None))));
        assert_eq!(shown.link, "");
    }

    #[test]
    fn hover_hints_stay_short() {
        for live in [None, live(true, Some(12)), live(false, None)] {
            let shown = arrangement("Laptop", Some(("Studio", Side::Below, live)));
            let (there, _) = shown.there.clone().unwrap();
            for hint in [&shown.here.hint, &there.hint, &shown.link_hint] {
                assert!(hint.split_whitespace().count() <= 5, "{hint:?}");
            }
        }
    }
}
