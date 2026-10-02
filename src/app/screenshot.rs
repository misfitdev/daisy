//! Draws Daisy's window offscreen with sample systems and saves it as a PNG,
//! for the website. Nothing is shared or saved, and no permission is needed.

use std::path::Path;

use anyhow::{Context, Result, bail};
use objc2::AnyThread;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::{
    NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSBitmapImageFileType, NSBitmapImageRep, NSView,
};
use objc2_foundation::{MainThreadMarker, NSDictionary, NSObject, NSString};

use super::map::Shown;
use super::window::{MainViews, PeerRow};
use crate::identity::PublicKey;
use crate::input::Rect;

/// Pixels per point in the saved image, as on a Retina display.
const SCALE: f64 = 2.0;

pub fn save(path: &Path) -> Result<()> {
    let mtm = MainThreadMarker::new().context("drawing Daisy's window must happen on the main thread")?;
    let target = NSObject::new();
    let views = MainViews::new(mtm, as_target(&target));
    // SAFETY: AppKit's constant appearance name
    let light = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua });
    views.window.setAppearance(light.as_deref());

    let (laptop, studio) = (key(1), key(2));
    views.arrange.show(vec![
        Shown {
            key: laptop,
            name: "Laptop".to_owned(),
            displays: vec![rect(0.0, 0.0, 1512.0, 982.0)],
            offset: (0.0, 0.0),
            me: true,
            in_control: Some(true),
            locked: false,
            hint: String::new(),
        },
        Shown {
            key: studio,
            name: "Studio".to_owned(),
            displays: vec![rect(0.0, 0.0, 2560.0, 1440.0), rect(2560.0, 260.0, 1920.0, 1080.0)],
            offset: (1512.0, -229.0),
            me: false,
            in_control: Some(false),
            locked: false,
            hint: String::new(),
        },
    ]);
    views.title.setStringValue(&NSString::from_str("Connected to Studio"));
    views.detail.setStringValue(&NSString::from_str("4 ms"));
    views.show_peers(
        &[PeerRow {
            name: "Studio".to_owned(),
            detail: "Connected, 4 ms".to_owned(),
            fingerprint: String::new(),
            trust: "Until unused for 4 days".to_owned(),
        }],
        as_target(&target),
    );
    views.start.setHidden(true);
    views.stop.setHidden(false);

    // the frame view draws the title bar too, as the window appears
    let content = views.window.contentView().context("the window has no content")?;
    // SAFETY: the window, and so its frame view, outlives this call
    let frame = unsafe { content.superview() }.unwrap_or(content);
    write_png(&frame, path)
}

fn write_png(view: &NSView, path: &Path) -> Result<()> {
    let bounds = view.bounds();
    view.layoutSubtreeIfNeeded();
    let rep = view
        .bitmapImageRepForCachingDisplayInRect(bounds)
        .context("AppKit could not make an image of the window")?;
    rep.setSize(bounds.size);
    let scaled = scaled_rep(&rep, bounds.size.width * SCALE, bounds.size.height * SCALE)?;
    view.cacheDisplayInRect_toBitmapImageRep(bounds, &scaled);
    // SAFETY: an empty property dictionary is valid for PNG
    let png = unsafe { scaled.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) }
        .context("AppKit could not encode the image")?;
    if !png.to_vec().is_empty() {
        std::fs::write(path, png.to_vec()).with_context(|| format!("writing {}", path.display()))?;
        return Ok(());
    }
    bail!("the image came out empty")
}

/// A bitmap of `width` by `height` pixels that draws at `like`'s size in points.
fn scaled_rep(like: &NSBitmapImageRep, width: f64, height: f64) -> Result<Retained<NSBitmapImageRep>> {
    // SAFETY: a null planes pointer asks AppKit to allocate the pixels
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            objc2_app_kit::NSDeviceRGBColorSpace,
            0,
            0,
        )
    }
    .context("AppKit could not make a bitmap")?;
    rep.setSize(like.size());
    Ok(rep)
}

fn as_target(object: &NSObject) -> &AnyObject {
    object.as_ref()
}

/// Sample keys, never used to connect.
fn key(byte: u8) -> PublicKey {
    PublicKey::from_bytes(&[byte; 32]).expect("any 32 bytes make a sample key")
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> Rect {
    Rect { x, y, width, height }
}
