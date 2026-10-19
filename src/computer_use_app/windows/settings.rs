//! Serialize Owner settings against native input, without stopping Actor workers.
use super::*;
use crate::computer_use_app::settings::{Patch, Preferences, Status};
use daat_locus_virtual_workspace::DisplayOptions;
use parking_lot::{Mutex, RwLock, RwLockReadGuard};
use std::sync::{Weak, atomic::AtomicU64};

pub(crate) struct Workspace {
    pub(super) display: SharedDisplay,
    gate: RwLock<()>,
    active: AtomicBool,
    generation: AtomicU64,
    clients: Mutex<Vec<Weak<Registration>>>,
    state: Mutex<(Preferences, bool)>,
    path: PathBuf,
}
impl Workspace {
    pub fn open(directory: &Path) -> Result<Arc<Self>> {
        let path = directory.join("computer-use.json");
        let preferences = Preferences::load(&path)?;
        Ok(Arc::new(Self {
            display: SharedDisplay::global().into_diagnostic()?,
            gate: RwLock::new(()),
            active: AtomicBool::new(false),
            generation: AtomicU64::new(0),
            clients: Mutex::new(vec![]),
            state: Mutex::new((preferences, false)),
            path,
        }))
    }
    pub fn status(&self) -> Status {
        let mut state = self.state.lock();
        self.refresh_health(&mut state);
        Status::new(state.0, state.1)
    }
    fn refresh_health(&self, state: &mut (Preferences, bool)) {
        if state.1
            && !self.display.inspect().is_ok_and(|display| {
                display.is_some_and(|display| {
                    display.bounds.width().ok() == Some(state.0.display.width as i32)
                        && display.bounds.height().ok() == Some(state.0.display.height as i32)
                })
            })
        {
            state.1 = false;
            self.active.store(false, Ordering::Release);
        }
    }
    pub fn initialize(&self) -> Result<()> {
        self.configure(Patch::default()).map(|_| ())
    }
    pub fn configure(&self, patch: Patch) -> Result<Status> {
        let mut state = self.state.lock();
        let previous = state.0;
        let candidate = previous.patched(patch)?;
        self.refresh_health(&mut state);
        let was_ready = state.1;
        if candidate == previous && (!candidate.enabled || state.1) {
            return Ok(Status::new(previous, state.1));
        }
        let _exclusive = self.gate.write();
        self.active.store(false, Ordering::Release);
        state.1 = false;
        self.generation.fetch_add(1, Ordering::AcqRel);
        // Revoke cached references before changing any monitor geometry. Reset
        // requests bypass the input gate and cannot replay an App operation.
        let clients = {
            let mut clients = self.clients.lock();
            clients.retain(|client| client.strong_count() != 0);
            clients.iter().filter_map(Weak::upgrade).collect::<Vec<_>>()
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut failures = vec![];
        for client in clients {
            let (tx, rx) = flume::bounded(1);
            let request = Request {
                call: None,
                context: None,
                reset: true,
                generation: 0,
                cancelled: Arc::new(AtomicBool::new(false)),
                reply: tx,
            };
            match client.requests.send_deadline(request, deadline) {
                Err(flume::SendTimeoutError::Disconnected(_)) => continue,
                Err(_) => {
                    failures.push("a desktop operation is still finishing".to_owned());
                    continue;
                }
                Ok(()) => {}
            }
            match rx.recv_deadline(deadline) {
                Ok(Ok(_)) => {}
                Ok(Err(error)) => failures.push(error.to_string()),
                Err(_) => failures.push("window handoff is still pending".to_owned()),
            }
        }
        if !failures.is_empty() {
            return Err(miette!(
                "GUI configuration pending: {}; retry after window handoff",
                failures.join("; ")
            ));
        }
        let options = |value: Preferences| DisplayOptions {
            width: value.display.width,
            height: value.display.height,
            ..Default::default()
        };
        self.display
            .configure(candidate.enabled, options(candidate))
            .into_diagnostic()?;
        if let Err(error) = candidate.save(&self.path) {
            if self
                .display
                .configure(previous.enabled && was_ready, options(previous))
                .is_ok()
            {
                state.1 = previous.enabled && was_ready;
                self.active.store(state.1, Ordering::Release);
            }
            return Err(error);
        }
        *state = (candidate, candidate.enabled);
        self.active.store(candidate.enabled, Ordering::Release);
        Ok(Status::new(candidate, state.1))
    }
    pub(super) fn permit(&self, generation: u64) -> Result<RwLockReadGuard<'_, ()>> {
        let guard = self
            .gate
            .try_read()
            .ok_or_else(|| miette!("GUI settings are changing; observe again"))?;
        if !self.active.load(Ordering::Acquire) {
            return Err(miette!("Enable or reconnect GUI operation in Settings"));
        }
        if generation != self.generation.load(Ordering::Acquire) {
            return Err(miette!(
                "GUI settings changed; observe again before operating"
            ));
        }
        Ok(guard)
    }
    pub(super) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub(super) fn register(&self, client: &Arc<Registration>) {
        self.clients.lock().push(Arc::downgrade(client));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use_app::settings::DisplaySettings;

    #[test]
    fn disabling_revokes_registered_hosts_and_persists_display_without_creating_an_output() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let (tx, rx) = flume::bounded::<Request>(1);
        let client = Arc::new(Registration { requests: tx });
        workspace.register(&client);
        let worker = std::thread::spawn(move || {
            let request = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(request.reset && request.call.is_none());
            request
                .reply
                .send(Ok(Reply::data("released", json!({}))))
                .unwrap();
        });
        let preferences = Preferences {
            enabled: false,
            display: DisplaySettings {
                width: 2560,
                height: 1440,
            },
        };
        workspace
            .configure(Patch {
                enabled: Some(false),
                display: Some(preferences.display),
            })
            .unwrap();
        worker.join().unwrap();
        assert!(workspace.permit(workspace.generation()).is_err());
        assert!(workspace.display.inspect().unwrap().is_none());
        assert!(workspace.display.reserve(960, 720).is_err());
        drop(workspace);
        let reopened = Workspace::open(directory.path()).unwrap();
        assert_eq!(
            serde_json::to_value(reopened.status()).unwrap()["enabled"],
            false
        );
        assert_eq!(
            Preferences::load(&directory.path().join("computer-use.json")).unwrap(),
            preferences
        );
    }

    #[test]
    fn validation_and_failed_persistence_preserve_previous_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let original = workspace.state.lock().0;
        assert!(
            workspace
                .configure(Patch {
                    enabled: Some(false),
                    display: Some(DisplaySettings {
                        width: 1921,
                        height: 1080
                    })
                })
                .is_err()
        );
        assert_eq!(workspace.state.lock().0, original);
        // Force the real atomic writer to fail at the final target, after native
        // disable completed. Rollback must not initialize an unavailable output.
        std::fs::create_dir(directory.path().join("computer-use.json")).unwrap();
        assert!(
            workspace
                .configure(Patch {
                    enabled: Some(false),
                    display: None
                })
                .is_err()
        );
        assert_eq!(workspace.state.lock().0, original);
        assert!(workspace.permit(workspace.generation()).is_err());
        assert!(workspace.display.inspect().unwrap().is_none());
    }

    #[test]
    fn configuration_gate_rejects_inflight_admission_and_queued_old_generations() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        workspace.active.store(true, Ordering::Release);
        let old = workspace.generation();
        {
            let _exclusive = workspace.gate.write();
            assert!(workspace.permit(old).is_err());
        }
        workspace
            .configure(Patch {
                enabled: Some(false),
                display: None,
            })
            .unwrap();
        // Isolate generation fencing from the separate disabled-state check.
        workspace.active.store(true, Ordering::Release);
        assert!(workspace.permit(old).is_err());
        assert!(workspace.permit(workspace.generation()).is_ok());
        workspace.active.store(false, Ordering::Release);
    }

    #[test]
    fn failed_window_handoff_fences_input_and_keeps_the_previous_preference_until_retry() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Workspace::open(directory.path()).unwrap();
        let (tx, rx) = flume::bounded::<Request>(1);
        let client = Arc::new(Registration { requests: tx });
        workspace.register(&client);
        let worker = std::thread::spawn(move || {
            let first = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(first.reset && first.call.is_none());
            first.reply.send(Err(miette!("handoff pending"))).unwrap();
            let second = rx.recv_timeout(Duration::from_secs(5)).unwrap();
            assert!(second.reset && second.call.is_none());
            second
                .reply
                .send(Ok(Reply::data("released", json!({}))))
                .unwrap();
        });
        assert!(
            workspace
                .configure(Patch {
                    enabled: Some(false),
                    display: None
                })
                .is_err()
        );
        assert!(workspace.state.lock().0.enabled);
        assert!(!directory.path().join("computer-use.json").exists());
        assert!(workspace.permit(workspace.generation()).is_err());
        workspace
            .configure(Patch {
                enabled: Some(false),
                display: None,
            })
            .unwrap();
        worker.join().unwrap();
        assert!(
            !Preferences::load(&directory.path().join("computer-use.json"))
                .unwrap()
                .enabled
        );
    }
}
