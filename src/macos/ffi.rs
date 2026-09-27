//! The CoreGraphics and CoreFoundation calls input sharing needs, with
//! constants from CGEventTypes.h and CGRemoteOperation.h.

#![allow(non_upper_case_globals)]

use std::ffi::c_void;

pub type CGEventRef = *mut c_void;
pub type CGEventSourceRef = *mut c_void;
pub type CFMachPortRef = *mut c_void;
pub type CFRunLoopRef = *mut c_void;
pub type CFRunLoopSourceRef = *mut c_void;
pub type CGDirectDisplayID = u32;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

pub type CGEventTapCallBack =
    extern "C" fn(proxy: *mut c_void, event_type: u32, event: CGEventRef, user_info: *mut c_void) -> CGEventRef;

// CGEventType
pub const kCGEventLeftMouseDown: u32 = 1;
pub const kCGEventLeftMouseUp: u32 = 2;
pub const kCGEventRightMouseDown: u32 = 3;
pub const kCGEventRightMouseUp: u32 = 4;
pub const kCGEventMouseMoved: u32 = 5;
pub const kCGEventLeftMouseDragged: u32 = 6;
pub const kCGEventRightMouseDragged: u32 = 7;
pub const kCGEventKeyDown: u32 = 10;
pub const kCGEventKeyUp: u32 = 11;
pub const kCGEventFlagsChanged: u32 = 12;
pub const kCGEventScrollWheel: u32 = 22;
pub const kCGEventOtherMouseDown: u32 = 25;
pub const kCGEventOtherMouseUp: u32 = 26;
pub const kCGEventOtherMouseDragged: u32 = 27;
pub const kCGEventTapDisabledByTimeout: u32 = 0xFFFF_FFFE;
pub const kCGEventTapDisabledByUserInput: u32 = 0xFFFF_FFFF;

// CGEventField
pub const kCGMouseEventNumber: u32 = 0;
pub const kCGMouseEventClickState: u32 = 1;
pub const kCGMouseEventButtonNumber: u32 = 3;
pub const kCGMouseEventDeltaX: u32 = 4;
pub const kCGMouseEventDeltaY: u32 = 5;
pub const kCGKeyboardEventAutorepeat: u32 = 8;
pub const kCGKeyboardEventKeycode: u32 = 9;
pub const kCGScrollWheelEventPointDeltaAxis1: u32 = 96;
pub const kCGScrollWheelEventPointDeltaAxis2: u32 = 97;

// CGEventTapLocation, CGEventTapPlacement, CGEventTapOptions
pub const kCGHIDEventTap: u32 = 0;
pub const kCGSessionEventTap: u32 = 1;
pub const kCGHeadInsertEventTap: u32 = 0;
pub const kCGEventTapOptionDefault: u32 = 0;

// CGEventSourceStateID
pub const kCGEventSourceStateHIDSystemState: i32 = 1;

// CGScrollEventUnit
pub const kCGScrollEventUnitPixel: u32 = 0;

// CFStringBuiltInEncodings
pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;
// CFNumberType
pub const kCFNumberDoubleType: isize = 13;

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub fn CGEventTapCreate(
        tap: u32,
        place: u32,
        options: u32,
        events_of_interest: u64,
        callback: CGEventTapCallBack,
        user_info: *mut c_void,
    ) -> CFMachPortRef;
    pub fn CGEventTapEnable(tap: CFMachPortRef, enable: bool);

    pub fn CGEventGetLocation(event: CGEventRef) -> CGPoint;
    pub fn CGEventGetIntegerValueField(event: CGEventRef, field: u32) -> i64;
    pub fn CGEventGetDoubleValueField(event: CGEventRef, field: u32) -> f64;
    pub fn CGEventSetIntegerValueField(event: CGEventRef, field: u32, value: i64);
    pub fn CGEventSetDoubleValueField(event: CGEventRef, field: u32, value: f64);
    pub fn CGEventGetTimestamp(event: CGEventRef) -> u64;
    pub fn CGEventCreate(source: CGEventSourceRef) -> CGEventRef;
    pub fn CGEventCreateData(allocator: *const c_void, event: CGEventRef) -> *const c_void;
    pub fn CGEventCreateFromData(allocator: *const c_void, data: *const c_void) -> CGEventRef;
    pub fn CGEventGetFlags(event: CGEventRef) -> u64;
    pub fn CGEventSetFlags(event: CGEventRef, flags: u64);
    pub fn CGEventSetType(event: CGEventRef, event_type: u32);

    pub fn CGEventCreateMouseEvent(
        source: CGEventSourceRef,
        mouse_type: u32,
        position: CGPoint,
        button: u32,
    ) -> CGEventRef;
    pub fn CGEventCreateKeyboardEvent(source: CGEventSourceRef, keycode: u16, key_down: bool) -> CGEventRef;
    pub fn CGEventCreateScrollWheelEvent2(
        source: CGEventSourceRef,
        units: u32,
        wheel_count: u32,
        wheel1: i32,
        wheel2: i32,
        wheel3: i32,
    ) -> CGEventRef;
    pub fn CGEventPost(tap: u32, event: CGEventRef);

    pub fn CGEventSourceCreate(state: i32) -> CGEventSourceRef;
    pub fn CGEventSourceCounterForEventType(state: i32, event_type: u32) -> u32;

    pub fn CGWarpMouseCursorPosition(point: CGPoint) -> i32;
    pub fn CGAssociateMouseAndMouseCursorPosition(connected: bool) -> i32;

    pub fn CGGetActiveDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> i32;
    pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayHideCursor(display: CGDirectDisplayID) -> i32;
    pub fn CGDisplayShowCursor(display: CGDirectDisplayID) -> i32;
    pub fn CGEventSourceSetLocalEventsSuppressionInterval(source: CGEventSourceRef, seconds: f64);

    // Private WindowServer calls required for background cursor control.
    pub fn _CGSDefaultConnection() -> i32;
    pub fn CGSSetConnectionProperty(connection: i32, target: i32, key: *const c_void, value: *const c_void) -> i32;
    // the pointer's scale is global and outlives the process that set it
    pub fn CGSGetCursorScale(connection: i32, scale: *mut f32) -> i32;
    pub fn CGSSetCursorScale(connection: i32, scale: f32) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFRunLoopCommonModes: *const c_void;
    pub static kCFBooleanTrue: *const c_void;
    pub fn CFStringCreateWithCString(
        allocator: *const c_void,
        text: *const std::ffi::c_char,
        encoding: u32,
    ) -> *const c_void;
    pub fn CFMachPortCreateRunLoopSource(
        allocator: *const c_void,
        port: CFMachPortRef,
        order: isize,
    ) -> CFRunLoopSourceRef;
    pub fn CFMachPortInvalidate(port: CFMachPortRef);
    pub fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    pub fn CFRunLoopAddSource(run_loop: CFRunLoopRef, source: CFRunLoopSourceRef, mode: *const c_void);
    pub fn CFRunLoopRun();
    pub fn CFRunLoopStop(run_loop: CFRunLoopRef);
    pub fn CFRelease(object: *const c_void);
    pub fn CFPreferencesCopyAppValue(key: *const c_void, application: *const c_void) -> *const c_void;
    pub fn CFGetTypeID(object: *const c_void) -> usize;
    pub fn CFNumberGetTypeID() -> usize;
    pub fn CFBooleanGetTypeID() -> usize;
    pub fn CFNumberGetValue(number: *const c_void, number_type: isize, value: *mut c_void) -> bool;
    pub fn CFBooleanGetValue(boolean: *const c_void) -> bool;
}
