//! Keyboard and mouse input as it travels between peers, and the rules for
//! moving control from one screen to the other.
//!
//! Nothing here touches macOS: the platform layer turns real events into
//! calls on [`Driver`] and [`Target`] and carries out what they decide, so
//! the decisions can be tested directly.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::swipe::{SwipePhase, SwipeStep};

/// Where the peer sits relative to this system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

impl std::fmt::Display for Side {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Side::Left => "left",
            Side::Right => "right",
            Side::Above => "top",
            Side::Below => "bottom",
        })
    }
}

impl Side {
    pub fn opposite(self) -> Side {
        match self {
            Side::Left => Side::Right,
            Side::Right => Side::Left,
            Side::Above => Side::Below,
            Side::Below => Side::Above,
        }
    }
}

/// Position along an edge, from its top or left end at 0 to its bottom or
/// right end at `u16::MAX`, so screens of different sizes line up
/// proportionally.
pub type Along = u16;

/// One piece of input, forwarded from the driving system.
///
/// Travels inside `protocol::Message`, so the same rule applies: within one
/// protocol version, only append variants.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InputEvent {
    /// Pointer motion in points, as the mouse or trackpad reported it.
    Motion { dx: f64, dy: f64 },
    /// `button` 0 is left, 1 right, 2 and up the others. `clicks` is the
    /// click count the driving system computed, so double-click timing follows
    /// its settings.
    Button { button: u8, down: bool, clicks: u8 },
    /// Scroll distance in points.
    Scroll { dx: f64, dy: f64 },
    /// `code` is the macOS virtual key code; `flags` the modifier flags.
    Key {
        code: u16,
        down: bool,
        repeat: bool,
        flags: u64,
    },
    /// A modifier key changed; `flags` is the new modifier state.
    Modifiers { code: u16, flags: u64 },
    /// One step of a multi-finger trackpad swipe.
    Swipe { step: SwipeStep },
}

pub type Point = (f64, f64);

/// Screen area in macOS global coordinates: origin at the top left of the
/// main display, y growing downward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

// The cursor stops within this distance of a screen edge.
const EDGE_SLOP: f64 = 1.5;

impl Rect {
    fn max_x(&self) -> f64 {
        self.x + self.width
    }

    fn max_y(&self) -> f64 {
        self.y + self.height
    }

    fn along(&self, side: Side, point: Point) -> Along {
        // the same span entry_point uses, so crossing back and forth does not drift
        let fraction = match side {
            Side::Left | Side::Right => (point.1 - self.y) / (self.height - 1.0),
            Side::Above | Side::Below => (point.0 - self.x) / (self.width - 1.0),
        };
        (fraction.clamp(0.0, 1.0) * f64::from(u16::MAX)).round() as Along
    }

    /// Whether `point`, already at `side`, is being pushed out through it.
    fn pushed_through(&self, side: Side, point: Point, delta: Point) -> bool {
        match side {
            Side::Left => point.0 <= self.x + EDGE_SLOP && delta.0 < 0.0,
            Side::Right => point.0 >= self.max_x() - EDGE_SLOP && delta.0 > 0.0,
            Side::Above => point.1 <= self.y + EDGE_SLOP && delta.1 < 0.0,
            Side::Below => point.1 >= self.max_y() - EDGE_SLOP && delta.1 > 0.0,
        }
    }

    /// Whether `point` has gone past `side`.
    fn beyond(&self, side: Side, point: Point) -> bool {
        match side {
            Side::Left => point.0 < self.x,
            Side::Right => point.0 >= self.max_x(),
            Side::Above => point.1 < self.y,
            Side::Below => point.1 >= self.max_y(),
        }
    }

    /// The point just inside `side`, at `along`.
    pub fn entry_point(&self, side: Side, along: Along) -> Point {
        let fraction = f64::from(along) / f64::from(u16::MAX);
        let inset = 2.0;
        match side {
            Side::Left => (self.x + inset, self.y + fraction * (self.height - 1.0)),
            Side::Right => (self.max_x() - inset, self.y + fraction * (self.height - 1.0)),
            Side::Above => (self.x + fraction * (self.width - 1.0), self.y + inset),
            Side::Below => (self.x + fraction * (self.width - 1.0), self.max_y() - inset),
        }
    }

    fn clamp(&self, point: Point) -> Point {
        (
            point.0.clamp(self.x, self.max_x() - 1.0),
            point.1.clamp(self.y, self.max_y() - 1.0),
        )
    }
}

/// What the driving system should do with one of its own input events.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    /// Deliver it to this system as usual.
    Local,
    /// Swallow it.
    Drop,
    /// Swallow it, freeze the pointer, and hand control to the peer.
    Enter { along: Along },
    /// Swallow it and send it to the peer.
    Forward(InputEvent),
    /// Swallow it, take control back here at once, and have the peer
    /// release everything.
    Reclaim,
}

/// Escape takes control back from anywhere with Control, Option and Command
/// held, even if the peer or the connection is misbehaving.
const ESCAPE_KEY: u16 = 53;
const ESCAPE_MODIFIERS: u64 = 0x0004_0000 | 0x0008_0000 | 0x0010_0000;

/// Decides, on the system with the keyboard, where each input event goes.
pub struct Driver {
    screen: Rect,
    side: Side,
    remote: bool,
    // pressed before control moved away, so their release must stay here
    local_keys: BTreeSet<u16>,
    local_modifiers: BTreeSet<u16>,
    remote_modifiers: BTreeSet<u16>,
    orphaned_modifiers: BTreeSet<u16>,
    local_buttons: BTreeSet<u8>,
    // for the swipe under way, whether it began while the peer had control
    swipe_forwarded: Option<bool>,
}

impl Driver {
    /// `side` is where the peer sits.
    pub fn new(screen: Rect, side: Side) -> Self {
        Self {
            screen,
            side,
            remote: false,
            local_keys: BTreeSet::new(),
            local_modifiers: BTreeSet::new(),
            remote_modifiers: BTreeSet::new(),
            orphaned_modifiers: BTreeSet::new(),
            local_buttons: BTreeSet::new(),
            swipe_forwarded: None,
        }
    }

    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// The pointer moved by `delta` and is now at `point`.
    pub fn motion(&mut self, point: Point, delta: Point) -> Route {
        if self.remote {
            return Route::Forward(InputEvent::Motion {
                dx: delta.0,
                dy: delta.1,
            });
        }
        // never cross while dragging: the drag would end up split across two systems
        if self.local_buttons.is_empty() && self.screen.pushed_through(self.side, point, delta) {
            self.remote = true;
            return Route::Enter {
                along: self.screen.along(self.side, point),
            };
        }
        Route::Local
    }

    pub fn button(&mut self, button: u8, down: bool, clicks: u8) -> Route {
        if !down && self.local_buttons.remove(&button) {
            return Route::Local;
        }
        if !self.remote {
            if down {
                self.local_buttons.insert(button);
            }
            return Route::Local;
        }
        Route::Forward(InputEvent::Button { button, down, clicks })
    }

    pub fn key(&mut self, code: u16, down: bool, repeat: bool, flags: u64) -> Route {
        if !self.remote {
            if down {
                self.local_keys.insert(code);
            } else {
                self.local_keys.remove(&code);
            }
            return Route::Local;
        }
        if down && code == ESCAPE_KEY && flags & ESCAPE_MODIFIERS == ESCAPE_MODIFIERS {
            self.remote = false;
            return Route::Reclaim;
        }
        if !down && self.local_keys.remove(&code) {
            return Route::Local;
        }
        if down && self.local_keys.contains(&code) {
            // auto-repeat of a key held since before crossing: this system owns it
            return Route::Drop;
        }
        Route::Forward(InputEvent::Key {
            code,
            down,
            repeat,
            flags,
        })
    }

    pub fn modifiers(&mut self, code: u16, flags: u64) -> Route {
        if !self.remote {
            if self.orphaned_modifiers.remove(&code) {
                return Route::Drop;
            }
            if modifier_mask(code).is_some() && !self.local_modifiers.remove(&code) {
                self.local_modifiers.insert(code);
            }
            return Route::Local;
        }
        if self.local_modifiers.remove(&code) {
            return Route::Local;
        }
        if self.orphaned_modifiers.remove(&code) {
            return Route::Drop;
        }
        if modifier_mask(code).is_some() && !self.remote_modifiers.remove(&code) {
            self.remote_modifiers.insert(code);
        }
        Route::Forward(InputEvent::Modifiers { code, flags })
    }

    /// Whether the swipe under way, or one starting now, happens on this system.
    pub fn swipe_is_local(&self) -> bool {
        !self.swipe_forwarded.unwrap_or(self.remote)
    }

    /// A step of a trackpad swipe. A swipe belongs to the system that had
    /// control when it began, so one under way stays put when control moves.
    pub fn swipe(&mut self, step: SwipeStep) -> Route {
        if step.phase == SwipePhase::Began {
            self.swipe_forwarded = Some(self.remote);
        }
        let forwarded = self.swipe_forwarded.unwrap_or(self.remote);
        if step.is_last() {
            self.swipe_forwarded = None;
        }
        match (forwarded, self.remote) {
            (true, true) => Route::Forward(InputEvent::Swipe { step }),
            // control came back mid-swipe, and the peer cancelled it
            (true, false) => Route::Drop,
            (false, _) => Route::Local,
        }
    }

    pub fn scroll(&mut self, dx: f64, dy: f64) -> Route {
        if self.remote {
            Route::Forward(InputEvent::Scroll { dx, dy })
        } else {
            Route::Local
        }
    }

    /// Control came back; returns where to put the pointer.
    pub fn leave(&mut self, along: Along) -> Point {
        self.remote = false;
        self.orphaned_modifiers.append(&mut self.remote_modifiers);
        self.screen.entry_point(self.side, along)
    }

    /// The peer is gone; take control back without moving the pointer.
    pub fn reclaim(&mut self) {
        self.remote = false;
        self.orphaned_modifiers.append(&mut self.remote_modifiers);
    }
}

/// What the following system should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Move the pointer. `dragging` is the lowest held button, if any.
    Move {
        to: Point,
        delta: Point,
        dragging: Option<u8>,
    },
    Button {
        button: u8,
        down: bool,
        clicks: u8,
        at: Point,
    },
    Scroll {
        dx: f64,
        dy: f64,
    },
    Key {
        code: u16,
        down: bool,
        repeat: bool,
        flags: u64,
    },
    Modifiers {
        code: u16,
        flags: u64,
    },
    Swipe {
        step: SwipeStep,
    },
    /// The pointer went back out; tell the driving system where.
    Leave {
        along: Along,
    },
}

/// Replays forwarded input on the following system.
pub struct Target {
    screen: Rect,
    // the edge facing the driving system
    exit: Side,
    pointer: Option<Point>,
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
    modifiers: BTreeSet<u16>,
    flags: u64,
    // the latest step of a swipe under way
    swipe: Option<SwipeStep>,
}

impl Target {
    /// `driver_side` is where the driving system said this one sits.
    pub fn new(screen: Rect, driver_side: Side) -> Self {
        Self {
            screen,
            exit: driver_side.opposite(),
            pointer: None,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            modifiers: BTreeSet::new(),
            flags: 0,
            swipe: None,
        }
    }

    pub fn is_active(&self) -> bool {
        self.pointer.is_some()
    }

    pub fn enter(&mut self, along: Along) -> Vec<Action> {
        let to = self.screen.entry_point(self.exit, along);
        self.pointer = Some(to);
        vec![Action::Move {
            to,
            delta: (0.0, 0.0),
            dragging: None,
        }]
    }

    pub fn input(&mut self, event: InputEvent) -> Vec<Action> {
        let Some(pointer) = self.pointer else {
            return Vec::new();
        };
        match event {
            InputEvent::Motion { dx, dy } => {
                let moved = (pointer.0 + dx, pointer.1 + dy);
                if self.buttons.is_empty() && self.screen.beyond(self.exit, moved) {
                    let along = self.screen.along(self.exit, self.screen.clamp(moved));
                    let mut actions = self.release_all();
                    self.pointer = None;
                    actions.push(Action::Leave { along });
                    return actions;
                }
                let to = self.screen.clamp(moved);
                self.pointer = Some(to);
                vec![Action::Move {
                    to,
                    delta: (dx, dy),
                    dragging: self.buttons.first().copied(),
                }]
            }
            InputEvent::Button { button, down, clicks } => {
                if down {
                    self.buttons.insert(button);
                } else if !self.buttons.remove(&button) {
                    return Vec::new();
                }
                vec![Action::Button {
                    button,
                    down,
                    clicks,
                    at: pointer,
                }]
            }
            InputEvent::Scroll { dx, dy } => vec![Action::Scroll { dx, dy }],
            InputEvent::Swipe { step } => {
                match (step.phase, self.swipe) {
                    (SwipePhase::Began, _) => self.swipe = Some(step),
                    // only replay a swipe from its beginning
                    (_, None) => return Vec::new(),
                    _ if step.is_last() => self.swipe = None,
                    _ => self.swipe = Some(step),
                }
                vec![Action::Swipe { step }]
            }
            InputEvent::Key {
                code,
                down,
                repeat,
                flags,
            } => {
                if down {
                    self.keys.insert(code);
                } else if !self.keys.remove(&code) {
                    return Vec::new();
                }
                self.flags = flags;
                vec![Action::Key {
                    code,
                    down,
                    repeat,
                    flags,
                }]
            }
            InputEvent::Modifiers { code, flags } => {
                if modifier_mask(code).is_some_and(|mask| flags & mask != 0) {
                    self.modifiers.insert(code);
                } else {
                    self.modifiers.remove(&code);
                }
                self.flags = flags;
                vec![Action::Modifiers { code, flags }]
            }
        }
    }

    /// The driving system took control back without the pointer leaving.
    pub fn reclaim(&mut self) -> Vec<Action> {
        let actions = self.release_all();
        self.pointer = None;
        actions
    }

    /// Release everything held, so nothing stays stuck down after control
    /// leaves or the connection drops.
    pub fn release_all(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        if let Some(at) = self.pointer {
            for button in std::mem::take(&mut self.buttons) {
                actions.push(Action::Button {
                    button,
                    down: false,
                    clicks: 1,
                    at,
                });
            }
        }
        for code in std::mem::take(&mut self.keys) {
            actions.push(Action::Key {
                code,
                down: false,
                repeat: false,
                flags: 0,
            });
        }
        for code in std::mem::take(&mut self.modifiers) {
            actions.push(Action::Modifiers { code, flags: 0 });
        }
        self.flags = 0;
        // a swipe left hanging would leave the Dock halfway between Spaces
        if let Some(step) = self.swipe.take() {
            actions.push(Action::Swipe {
                step: SwipeStep {
                    phase: SwipePhase::Cancelled,
                    velocity: 0.0,
                    ..step
                },
            });
        }
        actions
    }
}

/// The modifier flag a modifier key sets, from CGEventTypes.h. Caps Lock is
/// left out: it toggles, and releasing it would flip it.
fn modifier_mask(code: u16) -> Option<u64> {
    const SHIFT: u64 = 0x0002_0000;
    const CONTROL: u64 = 0x0004_0000;
    const OPTION: u64 = 0x0008_0000;
    const COMMAND: u64 = 0x0010_0000;
    const FUNCTION: u64 = 0x0080_0000;
    match code {
        56 | 60 => Some(SHIFT),
        59 | 62 => Some(CONTROL),
        58 | 61 => Some(OPTION),
        55 | 54 => Some(COMMAND),
        63 => Some(FUNCTION),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect {
        x: 0.0,
        y: 0.0,
        width: 1000.0,
        height: 500.0,
    };
    const COMMAND_KEY: u16 = 55;
    const COMMAND_FLAG: u64 = 0x0010_0000;

    #[test]
    fn input_event_variant_tags_are_stable() {
        let tags = [
            postcard::to_stdvec(&InputEvent::Motion { dx: 0.0, dy: 0.0 }).unwrap()[0],
            postcard::to_stdvec(&InputEvent::Button {
                button: 0,
                down: false,
                clicks: 0,
            })
            .unwrap()[0],
            postcard::to_stdvec(&InputEvent::Scroll { dx: 0.0, dy: 0.0 }).unwrap()[0],
            postcard::to_stdvec(&InputEvent::Key {
                code: 0,
                down: false,
                repeat: false,
                flags: 0,
            })
            .unwrap()[0],
            postcard::to_stdvec(&InputEvent::Modifiers { code: 0, flags: 0 }).unwrap()[0],
            postcard::to_stdvec(&InputEvent::Swipe {
                step: swipe(SwipePhase::Began, 0.0),
            })
            .unwrap()[0],
        ];
        assert_eq!(tags, [0, 1, 2, 3, 4, 5]);
    }
    const A_KEY: u16 = 0;

    fn entered_driver() -> Driver {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert!(matches!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Enter { .. }));
        driver
    }

    #[test]
    fn pushing_through_the_shared_edge_hands_over_control() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.motion((500.0, 250.0), (-3.0, 0.0)), Route::Local);
        // at the edge but moving away from it
        assert_eq!(driver.motion((0.0, 250.0), (2.0, 0.0)), Route::Local);
        // at the opposite edge, which leads nowhere
        assert_eq!(driver.motion((999.0, 250.0), (3.0, 0.0)), Route::Local);

        assert_eq!(
            driver.motion((0.0, 250.0), (-3.0, 0.0)),
            Route::Enter {
                along: SCREEN.along(Side::Left, (0.0, 250.0))
            }
        );
        assert!(driver.is_remote());
        assert_eq!(
            driver.motion((0.0, 250.0), (-1.0, 2.0)),
            Route::Forward(InputEvent::Motion { dx: -1.0, dy: 2.0 })
        );
    }

    #[test]
    fn every_side_can_hand_over() {
        for (side, point, delta) in [
            (Side::Left, (0.5, 10.0), (-1.0, 0.0)),
            (Side::Right, (999.0, 10.0), (1.0, 0.0)),
            (Side::Above, (10.0, 0.5), (0.0, -1.0)),
            (Side::Below, (10.0, 499.0), (0.0, 1.0)),
        ] {
            let mut driver = Driver::new(SCREEN, side);
            assert!(matches!(driver.motion(point, delta), Route::Enter { .. }), "{side:?}");
        }
    }

    #[test]
    fn does_not_hand_over_mid_drag() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.button(0, true, 1), Route::Local);
        assert_eq!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Local);
        assert_eq!(driver.button(0, false, 1), Route::Local);
        assert!(matches!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Enter { .. }));
    }

    #[test]
    fn keys_held_before_crossing_are_released_here() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.key(COMMAND_KEY, true, false, COMMAND_FLAG), Route::Local);
        assert!(matches!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Enter { .. }));

        // its auto-repeat and release stay on this system
        assert_eq!(driver.key(COMMAND_KEY, true, true, COMMAND_FLAG), Route::Drop);
        assert_eq!(driver.key(COMMAND_KEY, false, false, 0), Route::Local);
        // a key pressed after crossing goes over, press and release
        assert!(matches!(driver.key(A_KEY, true, false, 0), Route::Forward(_)));
        assert!(matches!(driver.key(A_KEY, false, false, 0), Route::Forward(_)));
    }

    #[test]
    fn modifiers_held_before_crossing_are_released_here() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.modifiers(COMMAND_KEY, COMMAND_FLAG), Route::Local);
        assert_eq!(driver.modifiers(54, COMMAND_FLAG), Route::Local);
        assert!(matches!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Enter { .. }));

        // Aggregate flags stay set when one of the left/right pair is released,
        // so ownership must be tracked by key code rather than by flags.
        assert_eq!(driver.modifiers(COMMAND_KEY, COMMAND_FLAG), Route::Local);
        assert_eq!(driver.modifiers(54, 0), Route::Local);

        assert!(matches!(
            driver.modifiers(56, 0x0002_0000),
            Route::Forward(InputEvent::Modifiers { .. })
        ));
        assert!(matches!(
            driver.modifiers(56, 0),
            Route::Forward(InputEvent::Modifiers { .. })
        ));
    }

    #[test]
    fn remote_modifier_release_after_reclaim_is_not_recorded_as_local() {
        let mut driver = entered_driver();
        assert!(matches!(
            driver.modifiers(COMMAND_KEY, COMMAND_FLAG),
            Route::Forward(InputEvent::Modifiers { .. })
        ));

        driver.reclaim();
        assert_eq!(driver.modifiers(COMMAND_KEY, 0), Route::Drop);

        assert_eq!(driver.modifiers(COMMAND_KEY, COMMAND_FLAG), Route::Local);
        assert_eq!(driver.modifiers(COMMAND_KEY, 0), Route::Local);
    }

    #[test]
    fn input_goes_over_only_while_remote() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.scroll(0.0, 5.0), Route::Local);
        assert_eq!(driver.modifiers(COMMAND_KEY, COMMAND_FLAG), Route::Local);

        let mut driver = entered_driver();
        assert_eq!(
            driver.scroll(0.0, 5.0),
            Route::Forward(InputEvent::Scroll { dx: 0.0, dy: 5.0 })
        );
        assert!(matches!(driver.button(1, true, 2), Route::Forward(_)));

        let back = driver.leave(0);
        assert!(!driver.is_remote());
        assert_eq!(back, SCREEN.entry_point(Side::Left, 0));
        assert_eq!(driver.scroll(0.0, 5.0), Route::Local);
    }

    #[test]
    fn escape_chord_takes_control_back() {
        let mut driver = entered_driver();
        // Escape alone, or with only some of the modifiers, goes across
        assert!(matches!(driver.key(ESCAPE_KEY, true, false, 0), Route::Forward(_)));
        assert!(matches!(
            driver.key(ESCAPE_KEY, true, false, COMMAND_FLAG),
            Route::Forward(_)
        ));

        assert_eq!(driver.key(ESCAPE_KEY, true, false, ESCAPE_MODIFIERS), Route::Reclaim);
        assert!(!driver.is_remote());
        // local again, so the pointer must be pushed across to go back
        assert_eq!(driver.scroll(0.0, 1.0), Route::Local);
    }

    #[test]
    fn escape_chord_does_nothing_while_local() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.key(ESCAPE_KEY, true, false, ESCAPE_MODIFIERS), Route::Local);
    }

    #[test]
    fn target_reclaim_releases_and_deactivates() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        target.input(InputEvent::Button {
            button: 1,
            down: true,
            clicks: 1,
        });
        let actions = target.reclaim();
        assert!(matches!(
            actions[..],
            [Action::Button {
                button: 1,
                down: false,
                ..
            }]
        ));
        assert!(!target.is_active());
        assert!(target.input(InputEvent::Scroll { dx: 0.0, dy: 1.0 }).is_empty());
    }

    fn swipe(phase: SwipePhase, progress: f64) -> SwipeStep {
        SwipeStep {
            axis: crate::swipe::SwipeAxis::Horizontal,
            phase,
            progress,
            velocity: 0.0,
        }
    }

    fn entered_target() -> Target {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        target
    }

    #[test]
    fn swipes_go_over_only_while_remote() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        assert_eq!(driver.swipe(swipe(SwipePhase::Began, 0.0)), Route::Local);
        assert_eq!(driver.swipe(swipe(SwipePhase::Ended, 1.0)), Route::Local);
        let mut driver = entered_driver();
        for step in [swipe(SwipePhase::Began, 0.0), swipe(SwipePhase::Changed, 0.4)] {
            assert_eq!(driver.swipe(step), Route::Forward(InputEvent::Swipe { step }));
        }
    }

    #[test]
    fn a_swipe_stays_with_the_system_it_began_on() {
        let mut driver = Driver::new(SCREEN, Side::Left);
        driver.swipe(swipe(SwipePhase::Began, 0.0));
        assert!(matches!(driver.motion((0.0, 250.0), (-3.0, 0.0)), Route::Enter { .. }));
        assert_eq!(driver.swipe(swipe(SwipePhase::Changed, 0.4)), Route::Local);
        assert_eq!(driver.swipe(swipe(SwipePhase::Ended, 0.9)), Route::Local);

        // one begun over there is not finished here after control comes back
        assert!(matches!(driver.swipe(swipe(SwipePhase::Began, 0.0)), Route::Forward(_)));
        driver.reclaim();
        assert_eq!(driver.swipe(swipe(SwipePhase::Changed, 0.4)), Route::Drop);
        assert_eq!(driver.swipe(swipe(SwipePhase::Ended, 0.9)), Route::Drop);
        assert_eq!(driver.swipe(swipe(SwipePhase::Began, 0.0)), Route::Local);
    }

    #[test]
    fn target_replays_swipes_from_their_beginning() {
        let mut target = entered_target();
        assert!(
            target
                .input(InputEvent::Swipe {
                    step: swipe(SwipePhase::Changed, 0.3)
                })
                .is_empty()
        );
        for step in [
            swipe(SwipePhase::Began, 0.0),
            swipe(SwipePhase::Changed, 0.3),
            swipe(SwipePhase::Ended, 0.8),
        ] {
            assert_eq!(target.input(InputEvent::Swipe { step }), vec![Action::Swipe { step }]);
        }
        assert!(target.release_all().is_empty(), "a finished swipe needs no cancelling");
    }

    #[test]
    fn a_swipe_under_way_is_cancelled_when_control_goes() {
        let mut target = entered_target();
        target.input(InputEvent::Swipe {
            step: swipe(SwipePhase::Began, 0.0),
        });
        target.input(InputEvent::Swipe {
            step: swipe(SwipePhase::Changed, 0.6),
        });
        assert_eq!(
            target.reclaim(),
            vec![Action::Swipe {
                step: swipe(SwipePhase::Cancelled, 0.6)
            }]
        );
    }

    #[test]
    fn reclaim_returns_control_when_the_peer_is_gone() {
        let mut driver = entered_driver();
        driver.reclaim();
        assert!(!driver.is_remote());
        assert_eq!(driver.key(A_KEY, true, false, 0), Route::Local);
    }

    #[test]
    fn target_enters_opposite_edge_at_the_same_proportion() {
        // the driver has the target on its left, so the target is entered from its right
        let target_screen = Rect {
            x: 0.0,
            y: 0.0,
            width: 2000.0,
            height: 1000.0,
        };
        let mut target = Target::new(target_screen, Side::Left);
        let actions = target.enter(u16::MAX / 2);
        let Action::Move { to, .. } = actions[0] else {
            panic!("expected a move, got {actions:?}")
        };
        assert!(to.0 > 1990.0, "entered at {to:?}");
        assert!((to.1 - 499.5).abs() < 1.0, "entered at {to:?}");
    }

    #[test]
    fn crossing_back_and_forth_does_not_drift() {
        for along in [0, 1, 12_345, u16::MAX / 2, u16::MAX - 1, u16::MAX] {
            for side in [Side::Left, Side::Right, Side::Above, Side::Below] {
                let point = SCREEN.entry_point(side, along);
                assert_eq!(SCREEN.along(side, point), along, "{side:?} at {along}");
            }
        }
    }

    #[test]
    fn target_ignores_input_until_entered() {
        let mut target = Target::new(SCREEN, Side::Left);
        assert!(target.input(InputEvent::Scroll { dx: 0.0, dy: 1.0 }).is_empty());
        assert!(!target.is_active());
    }

    #[test]
    fn target_moves_within_its_screen_and_leaves_through_the_driver_edge() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(u16::MAX / 2);

        let actions = target.input(InputEvent::Motion { dx: -100.0, dy: 0.0 });
        assert!(matches!(actions[0], Action::Move { to, .. } if (to.0 - 898.0).abs() < 0.01));

        // the far edges clamp instead of leaving
        let actions = target.input(InputEvent::Motion { dx: -5000.0, dy: 0.0 });
        assert!(matches!(actions[0], Action::Move { to, .. } if to.0 == 0.0));

        let actions = target.input(InputEvent::Motion { dx: 5000.0, dy: 0.0 });
        assert_eq!(actions, vec![Action::Leave { along: u16::MAX / 2 }]);
        assert!(!target.is_active());
    }

    #[test]
    fn target_drags_and_does_not_leave_mid_drag() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        target.input(InputEvent::Button {
            button: 0,
            down: true,
            clicks: 1,
        });
        let actions = target.input(InputEvent::Motion { dx: 5000.0, dy: 0.0 });
        assert!(matches!(actions[0], Action::Move { dragging: Some(0), .. }));
        assert!(target.is_active());
    }

    #[test]
    fn leaving_releases_everything_held() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        target.input(InputEvent::Modifiers {
            code: COMMAND_KEY,
            flags: COMMAND_FLAG,
        });
        target.input(InputEvent::Key {
            code: A_KEY,
            down: true,
            repeat: false,
            flags: COMMAND_FLAG,
        });

        let actions = target.input(InputEvent::Motion { dx: 5000.0, dy: 0.0 });
        assert_eq!(
            actions,
            vec![
                Action::Key {
                    code: A_KEY,
                    down: false,
                    repeat: false,
                    flags: 0
                },
                Action::Modifiers {
                    code: COMMAND_KEY,
                    flags: 0
                },
                Action::Leave { along: 0 },
            ]
        );
        assert!(target.release_all().is_empty(), "nothing left to release");
    }

    #[test]
    fn stray_releases_are_ignored() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        assert!(
            target
                .input(InputEvent::Key {
                    code: A_KEY,
                    down: false,
                    repeat: false,
                    flags: 0
                })
                .is_empty()
        );
        assert!(
            target
                .input(InputEvent::Button {
                    button: 0,
                    down: false,
                    clicks: 1
                })
                .is_empty()
        );
    }

    #[test]
    fn caps_lock_is_never_released() {
        let mut target = Target::new(SCREEN, Side::Left);
        target.enter(0);
        target.input(InputEvent::Modifiers {
            code: 57,
            flags: 0x0001_0000,
        });
        assert!(target.release_all().is_empty());
    }
}
