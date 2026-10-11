//! Minimal update host for the isolated native-framework installation check.
use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use objc2_foundation::NSTimer;
use sparkle_updater::{SparkleUpdater, UpdateEvent, UpdaterConfig};
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::Write;
use std::rc::Rc;
use std::time::Instant;

fn main() -> Result<()> {
    let executable = std::env::current_exe()?;
    let contents = executable.parent().unwrap().parent().unwrap();
    let info: plist::Value = plist::from_file(contents.join("Info.plist"))?;
    let log = info.as_dictionary().unwrap()["DaisySmokeLog"]
        .as_string()
        .unwrap()
        .to_owned();
    let version = info.as_dictionary().unwrap()["CFBundleVersion"].as_string().unwrap();
    let write = move |message: &str| {
        let mut file = OpenOptions::new().append(true).create(true).open(&log).unwrap();
        writeln!(file, "{message}").unwrap();
    };
    write(&format!("started {version} pid={}", std::process::id()));
    if version == "0.10.0" {
        return Ok(());
    }
    let mtm = MainThreadMarker::new().context("requires the main thread")?;
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let events = Rc::new(RefCell::new(Vec::new()));
    let received = events.clone();
    let manual = info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeManual")
        .and_then(plist::Value::as_boolean)
        == Some(true);
    let production = if manual {
        let updater = daisy::macos::update::Updater::new(mtm)?.context("fixture is not an application bundle")?;
        updater.configure(daisy::update::Policy::InstallMinorAndPatch, true)?;
        updater.check()?;
        Some(updater)
    } else {
        None
    };
    let updater = if manual {
        None
    } else {
        SparkleUpdater::new(
            mtm,
            UpdaterConfig {
                event_callback: Some(Rc::new(move |event| received.borrow_mut().push(event))),
                ..Default::default()
            },
        )?
    };
    let channel = info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeChannel")
        .and_then(plist::Value::as_string)
        .unwrap_or("stable");
    if let Some(updater) = &updater {
        updater.set_allowed_channels(Some(vec![channel.to_owned()]))?;
        updater.set_automatically_downloads_updates(true)?;
        updater.set_automatically_checks_for_updates(true)?;
        updater.check_for_updates_in_background()?;
    }
    let start = Instant::now();
    let app_for_timer = app.clone();
    let callback = RcBlock::new(move |_: std::ptr::NonNull<NSTimer>| {
        let pending = match &production {
            Some(updater) => updater.events(),
            None => events.borrow_mut().drain(..).collect(),
        };
        for event in pending {
            write(&format!("{} {}", event.name(), event.payload()));
            match event {
                UpdateEvent::DidFindValidUpdate(_) if manual => app_for_timer.terminate(None),
                UpdateEvent::WillInstallUpdateOnQuit(_) => app_for_timer.terminate(None),
                UpdateEvent::DidAbortWithError(_) => app_for_timer.terminate(None),
                _ => {}
            }
        }
        if start.elapsed().as_secs() > 45 {
            write("timeout");
            app_for_timer.terminate(None);
        }
    });
    // SAFETY: the repeating timer and all framework calls stay on the AppKit thread.
    let timer = unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.1, true, &callback) };
    app.run();
    timer.invalidate();
    drop(updater);
    Ok(())
}
