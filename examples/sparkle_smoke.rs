//! Minimal update host for the isolated native-framework installation check.
use anyhow::{Context, Result};
use block2::RcBlock;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSButton, NSView};
use objc2_foundation::NSTimer;
use sparkle_updater::{SparkleUpdater, UpdateEvent, UpdaterConfig};
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::io::Write;
use std::rc::Rc;
use std::time::Instant;

fn click_install(view: &NSView) -> bool {
    if let Some(button) = view.downcast_ref::<NSButton>()
        && button.isEnabled()
        && matches!(
            button.title().to_string().as_str(),
            "Install Update" | "Install and Relaunch" | "Install and Restart"
        )
    {
        // SAFETY: this fixture drives only its own native update controls on the AppKit thread.
        unsafe { button.performClick(None) };
        return true;
    }
    view.subviews().iter().any(|child| click_install(&child))
}

fn main() -> Result<()> {
    let executable = std::env::current_exe()?;
    let contents = executable.parent().unwrap().parent().unwrap();
    let info: plist::Value = plist::from_file(contents.join("Info.plist"))?;
    let log = info.as_dictionary().unwrap()["DaisySmokeLog"]
        .as_string()
        .unwrap()
        .to_owned();
    let version = info.as_dictionary().unwrap()["CFBundleVersion"].as_string().unwrap();
    let relaunch = info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeRelaunch")
        .and_then(plist::Value::as_boolean)
        == Some(true);
    let retry = info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeRetry")
        .and_then(plist::Value::as_boolean)
        == Some(true);
    let blocked_home = info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeFixtureUser")
        .and_then(plist::Value::as_string)
        .map(std::path::PathBuf::from);
    let write = move |message: &str| {
        let mut file = OpenOptions::new().append(true).create(true).open(&log).unwrap();
        writeln!(file, "{message}").unwrap();
    };
    write(&format!("started {version} pid={}", std::process::id()));
    if info
        .as_dictionary()
        .unwrap()
        .get("DaisySmokeReplacement")
        .and_then(plist::Value::as_boolean)
        == Some(true)
    {
        if relaunch {
            let directory = std::path::Path::new(
                info.as_dictionary().unwrap()["DaisySmokeRelaunchRoot"]
                    .as_string()
                    .unwrap(),
            );
            let restored = daisy::update::take_relaunch(directory, &executable, version, daisy::trust::now())?
                .context("replacement did not receive its selected profile")?;
            anyhow::ensure!(
                restored.home.to_str() == info.as_dictionary().unwrap()["DaisySmokeHome"].as_string(),
                "replacement selected the wrong home"
            );
            anyhow::ensure!(restored.name == "Update fixture", "replacement selected the wrong name");
            write("restored selected profile");
        }
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
    let production = if manual || relaunch {
        let home = if relaunch {
            let arguments: Vec<_> = std::env::args().collect();
            arguments
                .windows(2)
                .find(|pair| pair[0] == "--home")
                .map(|pair| pair[1].clone().into())
                .context("fixture needs --home")?
        } else {
            contents.join("fixture-home")
        };
        let launch = daisy::update::Launch {
            home,
            name: "Update fixture".into(),
        };
        let updater =
            daisy::macos::update::Updater::new(mtm, launch)?.context("fixture is not an application bundle")?;
        updater.configure(daisy::update::Policy::InstallMinorAndPatch, true)?;
        updater.check()?;
        Some(updater)
    } else {
        None
    };
    let updater = if manual || relaunch {
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
                UpdateEvent::WillInstallUpdateOnQuit(_) if !relaunch => app_for_timer.terminate(None),
                UpdateEvent::DidAbortWithError(_) => app_for_timer.terminate(None),
                _ => {}
            }
        }
        if relaunch {
            if let Some(updater) = production.as_ref()
                && let Some(error) = updater.take_relaunch_error()
            {
                write(&format!("relaunch save failed: {error}"));
                if retry {
                    std::fs::remove_file(blocked_home.as_ref().unwrap()).unwrap();
                    updater.check().unwrap();
                    write("retried relaunch after save failure");
                }
            }
            for window in app_for_timer.windows() {
                if window.isVisible()
                    && let Some(view) = window.contentView()
                {
                    click_install(&view);
                }
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
