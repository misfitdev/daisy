//! Native update framework integration. No custom download or installer process.
use crate::update::Policy;
use anyhow::Result;
use objc2::MainThreadMarker;
use sparkle_updater::{RelaunchContinuation, SparkleUpdater, UpdateEvent, UpdaterConfig};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

type SaveRelaunch = dyn Fn(&str) -> Result<()>;

pub struct Updater {
    native: SparkleUpdater,
    events: Rc<RefCell<VecDeque<UpdateEvent>>>,
    pending_relaunch: Rc<RefCell<crate::update::PendingRelaunch<RelaunchContinuation>>>,
    relaunch_error: Rc<RefCell<Option<String>>>,
    save_relaunch: Rc<SaveRelaunch>,
    mtm: MainThreadMarker,
}

impl Updater {
    pub fn new(mtm: MainThreadMarker, launch: crate::update::Launch) -> Result<Option<Self>> {
        let events = Rc::new(RefCell::new(VecDeque::new()));
        let received = events.clone();
        let directory = crate::controller::default_home()?;
        let executable = std::env::current_exe()?;
        let save: Rc<SaveRelaunch> = Rc::new(move |version: &str| {
            crate::update::save_relaunch(&directory, &executable, version, &launch, crate::trust::now())
        });
        let before_relaunch = save.clone();
        let save_relaunch = save.clone();
        let pending_relaunch = Rc::new(RefCell::new(crate::update::PendingRelaunch::default()));
        let pending = pending_relaunch.clone();
        let completed = pending_relaunch.clone();
        let relaunch_error = Rc::new(RefCell::new(None));
        let error_message = relaunch_error.clone();
        let version = RefCell::new(None::<String>);
        let native = SparkleUpdater::new(
            mtm,
            UpdaterConfig {
                event_callback: Some(Rc::new(move |event| {
                    match &event {
                        UpdateEvent::DidFindValidUpdate(info) => *version.borrow_mut() = Some(info.version.clone()),
                        UpdateEvent::WillRelaunchApplication => {
                            if let Some(version) = version.borrow().as_deref()
                                && let Err(error) = save(version)
                            {
                                tracing::error!(error = ?error, "update relaunch state could not be saved");
                            }
                        }
                        UpdateEvent::DidFinishUpdateCycle(_) | UpdateEvent::DidAbortWithError(_) => {
                            completed.borrow_mut().clear()
                        }
                        _ => {}
                    }
                    received.borrow_mut().push_back(event);
                })),
                relaunch_handler: Some(Rc::new(move |update, continuation| {
                    pending.borrow_mut().postpone(update.version, continuation);
                    let result = pending.borrow_mut().retry(|version| before_relaunch(version));
                    match result {
                        Ok(Some(continuation)) => continuation.resume(mtm),
                        Ok(None) => {}
                        Err(error) => {
                            tracing::error!(error = ?error, "update postponed: relaunch state could not be saved");
                            *error_message.borrow_mut() = Some(format!("{error:#}"));
                        }
                    }
                })),
                ..Default::default()
            },
        )?;
        Ok(native.map(|native| Self {
            native,
            events,
            pending_relaunch,
            relaunch_error,
            save_relaunch,
            mtm,
        }))
    }

    pub fn configure(&self, policy: Policy, automatic: bool) -> Result<()> {
        let version = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
        let protocols = crate::session::supported_protocols(&crate::session::Version::this_system());
        self.native.set_update_check_interval(6.0 * 60.0 * 60.0)?;
        self.native
            .set_allowed_channels(Some(vec![crate::update::channel(policy, &version, &protocols)]))?;
        self.native
            .set_automatically_downloads_updates(automatic && policy != Policy::NotifyOnly)?;
        self.native.set_automatically_checks_for_updates(automatic)?;
        Ok(())
    }

    pub fn check(&self) -> Result<()> {
        let pending = self
            .pending_relaunch
            .borrow_mut()
            .retry(|version| (self.save_relaunch)(version))?;
        if let Some(continuation) = pending {
            continuation.resume(self.mtm);
            return Ok(());
        }
        self.native.set_allowed_channels(Some(vec!["stable".to_owned()]))?;
        self.native.check_for_updates()?;
        Ok(())
    }
    pub fn events(&self) -> Vec<UpdateEvent> {
        self.events.borrow_mut().drain(..).collect()
    }

    pub fn take_relaunch_error(&self) -> Option<String> {
        self.relaunch_error.borrow_mut().take()
    }
}
