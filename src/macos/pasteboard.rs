//! The system pasteboard, for clipboard sharing. Carries out
//! `clipboard::Clipboard`; the decisions live in `clipboard`.

use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSBitmapImageRepPropertyKey, NSPasteboard, NSPasteboardTypePNG,
    NSPasteboardTypeRTF, NSPasteboardTypeString, NSPasteboardTypeTIFF,
};
use objc2_foundation::{NSData, NSDictionary, NSString};

use crate::clipboard::{Clipboard, Content, MAX_IMAGE};

const CONCEALED_TYPE: &str = "org.nspasteboard.ConcealedType";
const TRANSIENT_TYPE: &str = "org.nspasteboard.TransientType";

/// The general pasteboard. Looked up on every call rather than held, so this
/// stays `Send` and can live in the session task.
#[derive(Debug, Default, Clone, Copy)]
pub struct Pasteboard;

/// Screenshots and many apps copy images as TIFF only; Daisy carries PNG.
fn tiff_to_png(tiff: &NSData) -> Option<Vec<u8>> {
    let image = NSBitmapImageRep::imageRepWithData(tiff)?;
    let properties = NSDictionary::<NSBitmapImageRepPropertyKey, AnyObject>::new();
    // SAFETY: an empty properties dictionary is valid for every file type.
    let png = unsafe { image.representationUsingType_properties(NSBitmapImageFileType::PNG, &properties) }?;
    Some(png.to_vec())
}

impl Clipboard for Pasteboard {
    fn change_count(&self) -> i64 {
        NSPasteboard::generalPasteboard().changeCount() as i64
    }

    fn read(&self) -> Option<Content> {
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: AppKit initialises its pasteboard type constants before any
        // code runs and never changes them.
        let (string, rtf, png, tiff) = unsafe {
            (
                NSPasteboardTypeString,
                NSPasteboardTypeRTF,
                NSPasteboardTypePNG,
                NSPasteboardTypeTIFF,
            )
        };
        let concealed = board.types().is_some_and(|types| {
            types.iter().any(|kind| {
                let kind = kind.to_string();
                kind == CONCEALED_TYPE || kind == TRANSIENT_TYPE
            })
        });
        let content = Content {
            text: board.stringForType(string).map(|s| s.to_string()),
            rtf: board.dataForType(rtf).map(|d| d.to_vec()),
            png: board
                .dataForType(png)
                .filter(|d| d.length() <= MAX_IMAGE)
                .map(|d| d.to_vec())
                .or_else(|| {
                    board
                        .dataForType(tiff)
                        .filter(|d| d.length() <= MAX_IMAGE)
                        .and_then(|d| tiff_to_png(&d))
                }),
            concealed,
        };
        (!content.is_empty()).then_some(content)
    }

    fn write(&mut self, content: &Content) -> i64 {
        let board = NSPasteboard::generalPasteboard();
        // SAFETY: as in `read`.
        let (string, rtf, png) = unsafe { (NSPasteboardTypeString, NSPasteboardTypeRTF, NSPasteboardTypePNG) };
        board.clearContents();
        if let Some(text) = &content.text {
            board.setString_forType(&NSString::from_str(text), string);
        }
        if let Some(bytes) = &content.rtf {
            board.setData_forType(Some(&NSData::with_bytes(bytes)), rtf);
        }
        if let Some(bytes) = &content.png {
            board.setData_forType(Some(&NSData::with_bytes(bytes)), png);
        }
        board.changeCount() as i64
    }
}
