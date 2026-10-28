//! Actor identities, capability checks and asynchronous mailbox routing.
use super::{
    permissions::{canonical_path, normalize_config, protect_private_path},
    store::ActorStore,
    *,
};
use crate::sandbox::SandboxScopeRequest;
use crate::{
    app::{ActorCallContext, AppToolExecutionContext},
    artifacts::ArtifactReference,
    config::{Config, ProviderConfig, redact_secret_text, resolve_env_reference},
    core::ModelProvider,
    daat_locus_paths::daat_locus_paths_sync,
    model_protocol::{AgentContent, AgentContentPart},
};
use miette::{IntoDiagnostic, Result};
use parking_lot::{Mutex, RwLock};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::{sync::Notify, task::JoinHandle};
use uuid::Uuid;

pub trait ModelFactory: Send + Sync {
    fn build(&self, model_name: &str, config: &Config) -> Result<Arc<dyn ModelProvider>>;
}
struct Providers;
impl ModelFactory for Providers {
    fn build(&self, name: &str, config: &Config) -> Result<Arc<dyn ModelProvider>> {
        let provider: Box<dyn ModelProvider> =
            crate::providers::build_model_provider(name, config)?;
        Ok(Arc::from(provider))
    }
}
#[derive(Clone)]
pub struct ActorControl {
    pub wake: Arc<Notify>,
    pub stop: Arc<AtomicBool>,
    pub cancelled: Arc<Mutex<HashSet<String>>>,
}
impl Default for ActorControl {
    fn default() -> Self {
        Self {
            wake: Arc::new(Notify::new()),
            stop: Arc::new(AtomicBool::new(false)),
            cancelled: Arc::new(Mutex::new(HashSet::new())),
        }
    }
}
struct Worker {
    control: Arc<ActorControl>,
    task: JoinHandle<()>,
}
struct Inner {
    sudo_host: tokio::sync::Mutex<Option<Arc<crate::sudo_host::Client>>>,
    web_host: Arc<crate::web_host::Host>,
    temporary: Mutex<Option<crate::runtime_temp::RuntimeTemp>>,
    builtin_resources: Vec<crate::builtin_resources::BuiltinResource>,
    app_registry: Arc<crate::mcp::Registry>,
    instructions: crate::instruction_layers::InstructionStore,
    #[cfg(windows)]
    computer_workspace: Mutex<Option<Arc<crate::computer_use_app::Workspace>>>,
    workspace: PathBuf,
    store: ActorStore,
    config: RwLock<Config>,
    factory: Arc<dyn ModelFactory>,
    workers: Mutex<HashMap<String, Worker>>,
    metadata: Mutex<()>,
    lifecycle: tokio::sync::Mutex<()>,
    reconfiguring: AtomicBool,
    data_dir: PathBuf,
    delivery_dir: PathBuf,
    paused: AtomicBool,
    execution_suspended: AtomicBool,
    account_gated: AtomicBool,
    stopping: AtomicBool,
    idle_timeout: std::time::Duration,
    core: RwLock<Option<std::sync::Weak<crate::stage::shared_runtime::Controller>>>,
    local_core: AtomicBool,
}
#[derive(Clone)]
pub struct ActorSystem {
    inner: Arc<Inner>,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn invalid(message: &str) -> miette::Report {
    ActorError::Invalid(message.into()).into()
}
fn forbidden(message: &str) -> miette::Report {
    ActorError::Forbidden(message.into()).into()
}

impl ActorSystem {
    /// Called by the Windows service before readiness/worker activation. Ordinary
    /// ActorSystem construction stays independent of native desktop prerequisites.
    #[cfg(windows)]
    pub async fn prepare_computer_workspace(&self) -> Result<()> {
        let workspace = self.computer_workspace()?;
        tokio::task::spawn_blocking(move || workspace.initialize())
            .await
            .into_diagnostic()??;
        Ok(())
    }

    #[cfg(windows)]
    pub(crate) fn computer_workspace(&self) -> Result<Arc<crate::computer_use_app::Workspace>> {
        let mut workspace = self.inner.computer_workspace.lock();
        if workspace.is_none() {
            *workspace = Some(crate::computer_use_app::Workspace::open(
                &self.inner.data_dir,
            )?);
        }
        Ok(workspace
            .as_ref()
            .expect("initialized workspace service")
            .clone())
    }
    pub(crate) fn shared_core(&self) -> Option<Arc<crate::stage::shared_runtime::Controller>> {
        self.inner
            .core
            .read()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
    }
    pub(crate) fn install_local_core(&self) -> Result<()> {
        self.inner.core.write().take();
        self.store()
            .stage_transaction(|tx| {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='core_meta')",
                    [],
                    |row| row.get(0),
                )?;
                if exists {
                    tx.execute("UPDATE core_meta SET value='0' WHERE key='active'", [])?;
                    tx.execute("INSERT INTO core_meta VALUES('local_mode','1') ON CONFLICT(key) DO UPDATE SET value='1'",[])?;
                }
                Ok(())
            })
            .map_err(|error| miette::miette!("{error}"))?;
        self.inner.local_core.store(true, Ordering::Release);
        Ok(())
    }
    pub(crate) fn disable_local_core(&self) {
        self.inner.local_core.store(false, Ordering::Release);
    }
    pub(crate) fn local_core(&self) -> bool {
        self.inner.local_core.load(Ordering::Acquire)
    }
    pub(crate) fn core_permitted(&self) -> bool {
        if self.local_core() {
            self.workers_enabled()
                && !self.inner.stopping.load(Ordering::Acquire)
                && !self.inner.reconfiguring.load(Ordering::Acquire)
                && self.config().validate().is_ok()
        } else {
            self.shared_core().is_some_and(|core| core.permitted())
        }
    }
    pub(crate) fn core_accepts_input(&self) -> bool {
        if self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
        {
            return false;
        }
        if self.local_core() {
            self.core_permitted()
        } else {
            self.shared_core().is_some_and(|core| core.accepts_input())
        }
    }
    pub(crate) fn core_execution_permitted(&self) -> bool {
        if self.local_core() {
            self.core_permitted()
        } else {
            self.shared_core()
                .is_some_and(|core| core.execution_permitted())
        }
    }
    pub(crate) async fn checkpoint_core(&self) -> Result<()> {
        if self.local_core() {
            return Ok(());
        }
        self.shared_core()
            .ok_or_else(|| miette::miette!("Core worker unavailable"))?
            .checkpoint()
            .await
            .map_err(|error| miette::miette!("{error}"))
    }
    pub(crate) fn install_shared_core(&self, core: &Arc<crate::stage::shared_runtime::Controller>) {
        *self.inner.core.write() = Some(Arc::downgrade(core));
        self.wake(crate::stage::shared::ACTOR_ID);
    }
    pub(crate) fn ensure_core_actor(&self) -> Result<()> {
        if self.store().actor(crate::stage::shared::ACTOR_ID).is_err() {
            let actor = self.initial_core()?;
            self.prepare_dirs(&actor.id)?;
            self.store().save_actor(&actor)?;
        }
        self.start_worker(crate::stage::shared::ACTOR_ID)
    }
    /// The account Actor's identity is shared; filesystem/App installation
    /// policy belongs to the device exposing the instance.
    pub(crate) fn core_device_actor(&self) -> Result<ActorRecord> {
        let mut actor = self.actor(crate::stage::shared::ACTOR_ID)?;
        actor.config.workspace = self.device_workspace().to_owned();
        actor.config.apps = vec!["actors".into()];
        Ok(actor)
    }
    pub(crate) fn app_actor(&self, id: &str) -> Result<ActorRecord> {
        if id == crate::stage::shared::ACTOR_ID {
            self.core_device_actor()
        } else {
            self.actor(id)
        }
    }
    pub(crate) fn receive_stage_input(
        &self,
        peer: crate::stage::StageId,
        frame: &crate::stage::protocol::Envelope,
        content: String,
    ) -> Result<crate::stage::ledger::Receipt> {
        if self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
            || self.config().validate().is_err()
        {
            return Err(ActorError::Unavailable("Actor execution is unavailable".into()).into());
        }
        let local = self
            .store()
            .stage_local_id()
            .map_err(|error| miette::miette!("{error}"))?;
        let receipt = crate::stage::ledger::StageLedger::attach(self.store(), local)
            .accept_actor_input(peer, frame, content, now())
            .map_err(|error| miette::miette!("{error}"))?;
        if !receipt.duplicate {
            self.wake(&receipt.actor_id);
        }
        Ok(receipt)
    }
    pub(crate) fn execution_actor(&self, message: &MessageRecord) -> Result<ActorRecord> {
        let mut actor = self.actor(&message.actor_id)?;
        let external = matches!(
            message.source,
            MessageSource::RemoteUser { .. } | MessageSource::RemoteActor { .. }
        );
        if external && message.remote_authority.is_none() {
            return Err(forbidden("Remote input has no server-bound authority"));
        }
        if let Some(authority) = &message.remote_authority {
            let _peer = self
                .store()
                .stage_peer_for_authority(authority.peer)
                .map_err(|_| forbidden("Remote device authorization is unavailable or revoked"))?;
            let granted = if let Some(invocation) = authority.reply_for {
                crate::stage::delivery::reply_authority(
                    self.store(),
                    authority.peer,
                    invocation,
                    &actor.id,
                )
                .map_err(|_| forbidden("Remote reply is outside originating authority"))?;
                None
            } else {
                Some(
                    crate::stage::ledger::StageLedger::attach(
                        self.store(),
                        self.store()
                            .stage_local_id()
                            .map_err(|_| forbidden("Remote authority is unavailable"))?,
                    )
                    .authorize_actor(
                        authority.peer,
                        &authority.origin_actor,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|_| forbidden("Remote Actor capability is unavailable"))?,
                )
            };
            if authority.origin_actor != actor.id
                && granted.as_ref().is_none_or(|grant| !grant.delegate)
            {
                return Err(forbidden("Remote authority cannot delegate"));
            }
            actor.remote_read_only |=
                granted.as_ref().is_some_and(|grant| grant.read_only) || authority.read_only;
        }
        Ok(actor)
    }
    pub async fn open(data_dir: PathBuf, workspace: PathBuf, config: Config) -> Result<Self> {
        Self::open_with_factory(data_dir, workspace, config, Arc::new(Providers)).await
    }
    pub async fn open_account_gated(
        data_dir: PathBuf,
        workspace: PathBuf,
        config: Config,
    ) -> Result<Self> {
        let system = Self::build(data_dir, workspace, config, Arc::new(Providers), true).await?;
        system.inner.account_gated.store(true, Ordering::Release);
        Ok(system)
    }
    pub async fn activate_account_workers(&self) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        if !self.inner.account_gated.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        if self.inner.stopping.load(Ordering::Acquire) {
            return Err(invalid("Service is stopping"));
        }
        if self.inner.execution_suspended.load(Ordering::Acquire) {
            return Ok(());
        }
        self.inner.paused.store(false, Ordering::Release);
        self.start_queued_workers()?;
        self.store().changed().notify_waiters();
        Ok(())
    }
    pub fn workers_enabled(&self) -> bool {
        !self.inner.paused.load(Ordering::Acquire)
    }
    /// Android lifecycle suspends inference, preserving inboxes and interrupted
    /// effects. Device transport and account identity remain independent.
    pub async fn suspend_execution(&self) -> Result<()> {
        self.inner
            .execution_suspended
            .store(true, Ordering::Release);
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.inner.paused.store(true, Ordering::Release);
        let workers: Vec<_> = self.inner.workers.lock().drain().collect();
        for (_, worker) in &workers {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
        }
        for (id, worker) in workers {
            self.stop_actor_worker(&id, worker).await?;
        }
        Ok(())
    }
    pub async fn resume_execution(&self) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.inner
            .execution_suspended
            .store(false, Ordering::Release);
        if self.inner.account_gated.load(Ordering::Acquire)
            || self.inner.stopping.load(Ordering::Acquire)
        {
            return Ok(());
        }
        self.inner.paused.store(false, Ordering::Release);
        self.start_queued_workers()?;
        self.store().changed().notify_waiters();
        Ok(())
    }
    pub async fn open_with_factory(
        data_dir: PathBuf,
        workspace: PathBuf,
        config: Config,
        factory: Arc<dyn ModelFactory>,
    ) -> Result<Self> {
        Self::build(data_dir, workspace, config, factory, false).await
    }
    pub async fn open_paused_with_factory(
        data_dir: PathBuf,
        workspace: PathBuf,
        config: Config,
        factory: Arc<dyn ModelFactory>,
    ) -> Result<Self> {
        Self::build(data_dir, workspace, config, factory, true).await
    }
    async fn build(
        data_dir: PathBuf,
        workspace: PathBuf,
        config: Config,
        factory: Arc<dyn ModelFactory>,
        paused: bool,
    ) -> Result<Self> {
        let workspace = canonical_path(&workspace)?;
        let data_dir = canonical_path(&data_dir)?;
        std::fs::create_dir_all(&data_dir).into_diagnostic()?;
        protect_private_path(&data_dir)?;
        let delivery_dir = data_dir.with_file_name(format!(
            "{}-delivery",
            data_dir.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::create_dir_all(&delivery_dir).into_diagnostic()?;
        protect_private_path(&delivery_dir)?;
        let store = ActorStore::open(&data_dir.join("actors.sqlite3"))?;
        store.initialize_device_sandbox(&workspace)?;
        store.recover()?;
        let temporary =
            crate::runtime_temp::RuntimeTemp::create(&data_dir.with_file_name("runtime"))?;
        let builtin_resources = crate::builtin_resources::publish(&temporary)?;
        let system = Self {
            inner: Arc::new(Inner {
                sudo_host: tokio::sync::Mutex::new(None),
                web_host: Arc::new(crate::web_host::Host::default()),
                temporary: Mutex::new(Some(temporary)),
                builtin_resources,
                #[cfg(windows)]
                computer_workspace: Mutex::new(None),
                app_registry: crate::mcp::Registry::open(data_dir.join("apps.json"))?,
                instructions: crate::instruction_layers::InstructionStore::open(
                    &data_dir.join("account"),
                    store
                        .public_stage_id()
                        .map_err(|error| miette::miette!("{error}"))?,
                )?,
                workspace,
                store,
                config: RwLock::new(config),
                factory,
                workers: Mutex::new(HashMap::new()),
                metadata: Mutex::new(()),
                lifecycle: tokio::sync::Mutex::new(()),
                reconfiguring: AtomicBool::new(false),
                data_dir,
                delivery_dir,
                paused: AtomicBool::new(paused),
                execution_suspended: AtomicBool::new(false),
                account_gated: AtomicBool::new(false),
                stopping: AtomicBool::new(false),
                idle_timeout: std::time::Duration::from_secs(60),
                core: RwLock::new(None),
                local_core: AtomicBool::new(false),
            }),
        };
        if system
            .store()
            .actor(crate::stage::shared::ACTOR_ID)
            .is_err()
        {
            let record = system.initial_core()?;
            system.prepare_dirs(&record.id)?;
            system.store().save_actor(&record)?;
        }
        // Installation is device-wide. Retired capability lists are ignored.
        for mut actor in system.store().actors()? {
            let previous = actor.config.clone();
            if actor.id == crate::stage::shared::ACTOR_ID {
                actor.config.apps = vec!["actors".into()];
            } else {
                actor.config = system.hosted_config(actor.config)?;
            }
            if actor.config != previous {
                system.store().save_actor(&actor)?;
            }
        }
        system.start_queued_workers()?;
        Ok(system)
    }
    pub fn store(&self) -> &ActorStore {
        &self.inner.store
    }
    pub fn web_host(&self) -> Arc<crate::web_host::Host> {
        self.inner.web_host.clone()
    }
    pub fn app_registry(&self) -> Arc<crate::mcp::Registry> {
        self.inner.app_registry.clone()
    }
    pub fn device_workspace(&self) -> &Path {
        &self.inner.workspace
    }
    #[cfg(test)]
    pub(crate) fn seed_test_actor(&self, id: &str) {
        let mut actor = self.actor(crate::stage::shared::ACTOR_ID).unwrap();
        actor.id = id.to_owned();
        actor.parent_id = Some(crate::stage::shared::ACTOR_ID.into());
        actor.depth = 1;
        actor.config.name = format!("Fixture {id}");
        actor.config.apps = crate::builtin_apps::DEVICE_APPS
            .iter()
            .map(|app| (*app).to_owned())
            .collect();
        self.prepare_dirs(id).unwrap();
        self.store().save_actor(&actor).unwrap();
    }
    fn hosted_config(&self, mut config: ActorConfig) -> Result<ActorConfig> {
        config.apps = crate::builtin_apps::DEVICE_APPS
            .iter()
            .map(|app| (*app).to_owned())
            .collect();
        normalize_config(config)
    }
    pub fn infrastructure_dir(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }
    pub fn message_stage_from_actor(
        &self,
        sender: &str,
        current_id: &str,
        target: crate::stage::ActorAddress,
        content: String,
        call_id: &str,
        related: Option<&str>,
    ) -> Result<crate::stage::protocol::Envelope> {
        let current = self.store().message(current_id)?;
        if current.actor_id != sender
            || current.status != MessageStatus::Running
            || current.cancel_requested
        {
            return Err(forbidden("Actor message authority is not valid"));
        }
        let source = self.execution_actor(&current)?;
        self.ensure_delegation_authority(&current)?;
        let local = self
            .store()
            .stage_local_id()
            .map_err(|error| miette::miette!("{error}"))?;
        crate::stage::delivery::Deliveries::new(self.store(), local)
            .queue_related(
                &source,
                Some(&current),
                target,
                (content, Vec::new()),
                now(),
                call_id,
                related,
            )
            .map_err(|error| miette::miette!("{error}"))
    }
    pub fn config(&self) -> Config {
        self.inner.config.read().clone()
    }
    pub fn actor(&self, id: &str) -> Result<ActorRecord> {
        self.store().actor(id)
    }
    pub fn actor_data_dir(&self, id: &str) -> PathBuf {
        self.inner.delivery_dir.join(id)
    }
    pub fn account_credentials_dir(&self) -> PathBuf {
        self.inner.data_dir.join("account")
    }
    pub fn instructions(&self) -> &crate::instruction_layers::InstructionStore {
        &self.inner.instructions
    }
    pub(crate) async fn sudo_administrator(&self) -> Result<Arc<crate::sudo_host::Client>> {
        let mut host = self.inner.sudo_host.lock().await;
        if let Some(client) = host.as_ref().filter(|client| !client.is_closed()) {
            return Ok(client.clone());
        }
        let client = crate::sudo_host::elevate().await?;
        *host = Some(client.clone());
        Ok(client)
    }

    pub fn actor_state_dir(&self, id: &str) -> PathBuf {
        self.inner.data_dir.join("apps").join(id)
    }
    pub(crate) fn remove_reset_app_data(&self) -> Result<()> {
        // Both directories are framework-owned, outside all Actor workspaces.
        // std::fs::remove_dir_all does not follow symlinks/reparse points.
        for path in [
            self.inner.data_dir.join("apps"),
            self.inner.delivery_dir.clone(),
        ] {
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).into_diagnostic(),
            }
        }
        self.prepare_dirs(crate::stage::shared::ACTOR_ID)
    }
    pub(crate) async fn reset_account_work(
        &self,
        record: &crate::stage::reset::Record,
    ) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let pause = Reconfiguration::begin(&self.inner.reconfiguring, &self.inner.metadata);
        let root = self.initial_core()?;
        let workers: Vec<_> = self.inner.workers.lock().drain().collect();
        for (_, worker) in &workers {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
        }
        for (id, worker) in workers {
            self.stop_actor_worker(&id, worker).await?;
        }
        #[cfg(target_os = "android")]
        crate::computer_use_app::android::reset_resources()?;
        crate::stage::reset::replace(self.store(), record, &root, self.device_workspace())
            .map_err(|error| miette::miette!("{error}"))?;
        crate::stage::reset::cleanup(self).map_err(|error| miette::miette!("{error}"))?;
        drop(pause);
        Ok(())
    }
    fn initial_core(&self) -> Result<ActorRecord> {
        let workspace = self.inner.workspace.clone();
        let root = ActorRecord {
            remote_read_only: false,
            id: crate::stage::shared::ACTOR_ID.into(),
            parent_id: None,
            depth: 0,
            config: normalize_config(ActorConfig {
                name: "Daat Locus".into(),
                workspace,
                apps: vec!["actors".into()],
                model: None,
                instructions: String::new(),
            })?,
            context: Default::default(),
            status: ActorStatus::Idle,
            created_at_ms: now(),
            updated_at_ms: now(),
        };
        self.validate_config(&root.config)?;
        std::fs::create_dir_all(&root.config.workspace).into_diagnostic()?;
        Ok(root)
    }
    fn prepare_dirs(&self, id: &str) -> Result<()> {
        for directory in [self.actor_state_dir(id), self.actor_data_dir(id)] {
            std::fs::create_dir_all(&directory).into_diagnostic()?;
            protect_private_path(&directory)?;
        }
        Ok(())
    }
    fn start_worker(&self, id: &str) -> Result<()> {
        // Serialize activation with registry removal/draining. The flags must
        // be checked under this lock so shutdown cannot miss a new worker.
        let mut workers = self.inner.workers.lock();
        if self.inner.paused.load(Ordering::Acquire)
            || self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
            || self.inner.config.read().validate().is_err()
        {
            return Ok(());
        }
        if let Some(worker) = workers.get(id) {
            worker.control.wake.notify_one();
            return Ok(());
        }
        if !self.store().has_queued_input(id)?
            || (id == crate::stage::shared::ACTOR_ID && !self.core_permitted())
        {
            return Ok(());
        }
        self.prepare_dirs(id)?;
        let control = Arc::new(ActorControl::default());
        let system = self.clone();
        let actor_id = id.to_owned();
        let worker_control = control.clone();
        let task = tokio::spawn(async move {
            let passivated =
                super::runner::run_actor(system.clone(), actor_id.clone(), worker_control.clone())
                    .await;
            system.worker_finished(&actor_id, &worker_control, passivated);
        });
        workers.insert(id.into(), Worker { control, task });
        Ok(())
    }
    fn start_queued_workers(&self) -> Result<()> {
        for id in self.store().queued_actor_ids()? {
            self.start_worker(&id)?;
        }
        Ok(())
    }
    pub(crate) fn idle_timeout(&self) -> std::time::Duration {
        self.inner.idle_timeout
    }
    #[cfg(test)]
    pub(crate) fn set_idle_timeout(&mut self, timeout: std::time::Duration) {
        Arc::get_mut(&mut self.inner)
            .expect("No workers or other owners yet")
            .idle_timeout = timeout;
    }
    #[cfg(test)]
    pub(crate) fn resident_workers(&self) -> usize {
        self.inner.workers.lock().len()
    }
    fn worker_finished(&self, id: &str, control: &Arc<ActorControl>, passivated: bool) {
        {
            let mut workers = self.inner.workers.lock();
            // A lifecycle operation may already own this worker and its repair.
            if !workers
                .get(id)
                .is_some_and(|worker| Arc::ptr_eq(&worker.control, control))
            {
                return;
            }
            if id == crate::stage::shared::ACTOR_ID
                && self
                    .store()
                    .running_message(id)
                    .is_ok_and(|message| message.is_some())
                && let Some(core) = self.shared_core()
            {
                core.worker_stopped();
            }
            workers.remove(id);
            // Release retained HashMap capacity too after a burst of activity.
            if workers.capacity() > 16 && workers.len() < workers.capacity() / 4 {
                workers.shrink_to_fit();
            }
        }
        // A message can arrive during asynchronous App shutdown. Its wake may
        // have reached the retiring worker; the durable inbox closes that race.
        if passivated || control.stop.load(Ordering::Acquire) {
            self.wake(id);
        }
    }
    pub fn wake(&self, id: &str) {
        if self.start_worker(id).is_err() {
            tracing::error!(actor_id = id, "Actor worker could not be activated");
        }
    }
    pub fn provider(&self, actor: &ActorRecord) -> Result<Arc<dyn ModelProvider>> {
        let config = self.config();
        if config.validate().is_err() {
            return Err(ActorError::Unavailable("Agent configuration is not ready".into()).into());
        }
        self.inner.factory.build(
            config.actor_model_name(actor.config.model.as_deref()),
            &config,
        )
    }

    pub fn approval_provider(&self) -> Result<Arc<dyn ModelProvider>> {
        let config = self.config();
        self.inner.factory.build(
            config
                .approval_model
                .as_deref()
                .unwrap_or(&config.main_model),
            &config,
        )
    }

    pub fn requested_tool_context(
        &self,
        base: &AppToolExecutionContext,
        requested: &SandboxScopeRequest,
    ) -> Result<AppToolExecutionContext> {
        let call = base
            .actor_call
            .as_ref()
            .ok_or_else(|| forbidden("Missing tool caller authority"))?;
        let actor = self.record_for_tool(&call.actor_id, call)?;
        if actor.remote_read_only && !requested.writable_roots.is_empty() {
            return Err(forbidden(
                "Remote read-only authority cannot obtain write access",
            ));
        }
        let mut context = base.clone();
        let remote = self
            .store()
            .message(&call.message_id)?
            .remote_authority
            .is_some();
        for path in &requested.readable_roots {
            let resolved = canonical_path(path)?;
            if resolved != *path {
                return Err(forbidden(
                    "Requested filesystem target changed; request its current scope again",
                ));
            }
            let path = resolved;
            if context
                .sandbox_policy
                .filesystem
                .deny_read_paths
                .iter()
                .any(|denied| path.starts_with(denied))
            {
                return Err(forbidden(
                    "Requested filesystem scope conflicts with the current sandbox policy",
                ));
            }
            if remote && !base.sandbox_policy.is_path_readable(&path) {
                return Err(forbidden(
                    "External authority cannot expand the Actor's filesystem access",
                ));
            }
            context
                .sandbox_policy
                .filesystem
                .bound_roots
                .push(path.clone());
            context.sandbox_policy.filesystem.readable_roots.push(path);
        }
        for path in &requested.writable_roots {
            let resolved = canonical_path(path)?;
            if resolved != *path {
                return Err(forbidden(
                    "Requested filesystem target changed; request its current scope again",
                ));
            }
            let path = resolved;
            if context
                .sandbox_policy
                .filesystem
                .deny_read_paths
                .iter()
                .chain(context.sandbox_policy.filesystem.deny_write_paths.iter())
                .any(|denied| path.starts_with(denied))
            {
                return Err(forbidden(
                    "Requested filesystem scope conflicts with the current sandbox policy",
                ));
            }
            if remote && !base.sandbox_policy.is_path_writable(&path) {
                return Err(forbidden(
                    "External authority cannot expand the Actor's filesystem access",
                ));
            }
            context
                .sandbox_policy
                .filesystem
                .readable_roots
                .push(path.clone());
            context
                .sandbox_policy
                .filesystem
                .writable_roots
                .push(crate::sandbox::WritableRoot {
                    root: path.clone(),
                    read_only_subpaths: vec![],
                });
            context.sandbox_policy.filesystem.bound_roots.push(path);
        }
        context.sandbox_policy.filesystem.readable_roots.sort();
        context.sandbox_policy.filesystem.readable_roots.dedup();
        Ok(context)
    }
    fn validate_config(&self, config: &ActorConfig) -> Result<()> {
        if let Some(model) = &config.model
            && !self.inner.config.read().models.contains_key(model)
        {
            return Err(invalid("Unknown model configuration"));
        }
        Ok(())
    }
    fn create(
        &self,
        parent: &ActorRecord,
        config: ActorConfig,
        workspace_policy: Option<&crate::sandbox::RuntimeSandboxPolicy>,
        imported: Option<(&crate::resources::Resource, &str)>,
        initial_scopes: &[SandboxScope],
        source_message: Option<&str>,
    ) -> Result<ActorRecord> {
        if cfg!(target_os = "android") && config.apps.iter().any(|app| app == "terminal") {
            return Err(invalid("Terminal is unavailable on this device"));
        }
        let _metadata = self.inner.metadata.lock();
        if self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
        {
            return Err(ActorError::Unavailable("Actor configuration is changing".into()).into());
        }
        let import_id = imported.map(|(_, message)| {
            let hash = crate::stage::digest(
                format!("conversation-import:{}:{message}", parent.id).as_bytes(),
            );
            let bytes = hex::decode(hash).expect("SHA256 hex");
            Uuid::from_bytes(bytes[..16].try_into().expect("SHA256 prefix")).to_string()
        });
        if let Some(id) = &import_id
            && let Ok(actor) = self.store().actor(id)
        {
            return Ok(actor);
        }
        let mut config = self.hosted_config(config)?;
        config.name = config.name.trim().to_owned();
        self.validate_config(&config)?;
        self.store()
            .validate_new_actor_name(&parent.id, &config.name)?;
        crate::file_io::ensure_directory(
            workspace_policy.unwrap_or(&crate::sandbox::RuntimeSandboxPolicy::disabled()),
            &config.workspace,
        )?;
        let actor = ActorRecord {
            remote_read_only: false,
            context: Default::default(),
            id: import_id.unwrap_or_else(|| Uuid::new_v4().to_string()),
            parent_id: Some(parent.id.clone()),
            depth: parent
                .depth
                .checked_add(1)
                .ok_or_else(|| invalid("Actor depth overflow"))?,
            config,
            status: ActorStatus::Idle,
            created_at_ms: now(),
            updated_at_ms: now(),
        };
        self.prepare_dirs(&actor.id)?;
        let evidence = if let Some((resource, _)) = imported {
            Some(crate::conversation_import::prepare_evidence(
                self, &parent.id, &actor.id, resource,
            )?)
        } else {
            None
        };
        self.store()
            .save_created_actor(&actor, evidence, initial_scopes, source_message)?;
        Ok(actor)
    }
    pub fn create_owned(&self, parent: &str, config: ActorConfig) -> Result<ActorRecord> {
        let parent = self.actor(parent)?;
        self.create(&parent, config, None, None, &[], None)
    }
    pub async fn create_from_actor(
        &self,
        sender: &str,
        config: ActorConfig,
        initial_authorization: SandboxScopeRequest,
        context: &AppToolExecutionContext,
    ) -> Result<ActorRecord> {
        self.create_from_actor_with_resource(sender, config, initial_authorization, context, None)
            .await
    }
    pub(crate) async fn import_from_actor(
        &self,
        sender: &str,
        config: ActorConfig,
        resource: &crate::resources::Resource,
        context: &AppToolExecutionContext,
    ) -> Result<ActorRecord> {
        self.create_from_actor_with_resource(
            sender,
            config,
            SandboxScopeRequest::default(),
            context,
            Some(resource),
        )
        .await
    }
    async fn create_from_actor_with_resource(
        &self,
        sender: &str,
        config: ActorConfig,
        mut initial_authorization: SandboxScopeRequest,
        context: &AppToolExecutionContext,
        resource: Option<&crate::resources::Resource>,
    ) -> Result<ActorRecord> {
        let parent = self.app_actor(sender)?;
        self.store()
            .current_message(sender)?
            .ok_or_else(|| forbidden("Actor operations require an active request"))?;

        let config = self.hosted_config(config)?;
        let call = context
            .actor_call
            .as_ref()
            .ok_or_else(|| forbidden("Missing Actor creation authority"))?;
        let caller = self.record_for_tool(sender, call)?;
        if caller.remote_read_only {
            return Err(forbidden("Remote read-only authority cannot create Actors"));
        }
        if self
            .store()
            .message(&call.message_id)?
            .remote_authority
            .as_ref()
            .is_some_and(|authority| authority.reply_for.is_none())
        {
            return Err(forbidden("External requests cannot create Actors"));
        }
        self.validate_config(&config)?;
        // Imports resolve their stable input identity before checking the name in create.
        if resource.is_none() {
            self.store().validate_new_actor_name(sender, &config.name)?;
        }
        initial_authorization
            .writable_roots
            .push(config.workspace.clone());
        let initial_authorization = initial_authorization.canonicalized()?;
        let core = sender == crate::stage::shared::ACTOR_ID;
        let mut initial_scopes = vec![];
        let policy = if core {
            // This is Core's explicit device grant, not a per-App review bypass.
            let mut creation = self.requested_tool_context(context, &initial_authorization)?;
            for (roots, write) in [
                (&initial_authorization.readable_roots, false),
                (&initial_authorization.writable_roots, true),
            ] {
                for root in roots {
                    initial_scopes.push(SandboxScope::filesystem(root, write)?);
                }
            }
            if !config.workspace.exists() {
                // Preparing a nested cwd may create ancestors. This temporary
                // creation authority is never remembered as a parent grant.
                let preparation = SandboxScopeRequest {
                    readable_roots: vec![],
                    writable_roots: vec![
                        crate::sandbox::access::first_missing_directory(&config.workspace)
                            .to_owned(),
                    ],
                };
                creation = self.requested_tool_context(&creation, &preparation)?;
            }
            creation.sandbox_policy
        } else {
            let mut authorization = context.clone();
            if authorization.sandbox_policy.is_unrestricted() {
                let source = self.store().message(&call.message_id)?;
                authorization.sandbox_policy =
                    self.execution_context(&caller, &source)?.sandbox_policy;
            }
            let policy = authorization
                .files()
                .authorize_roots(&initial_authorization)
                .await?;
            let mut creation = context.clone();
            if !creation.sandbox_policy.is_unrestricted() {
                creation.sandbox_policy = policy;
            }
            creation.files().ensure_directory(&config.workspace).await?
        };
        self.record_for_tool(sender, call)?;
        self.create(
            &parent,
            config,
            Some(&policy),
            resource.map(|resource| (resource, call.message_id.as_str())),
            &initial_scopes,
            Some(&call.message_id),
        )
    }
    pub fn list_visible(&self) -> Result<Vec<ActorSummary>> {
        Ok(self
            .store()
            .actors()?
            .into_iter()
            .map(|a| a.summary())
            .collect())
    }
    pub(crate) async fn archive_from_core(
        &self,
        target: &str,
        archived: bool,
        caller: &ActorCallContext,
    ) -> Result<ActorRecord> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let core = self.record_for_tool(crate::stage::shared::ACTOR_ID, caller)?;
        let input = self.store().message(&caller.message_id)?;
        if core.remote_read_only
            || !matches!(input.source, MessageSource::User)
            || input.remote_authority.is_some()
            || crate::conversation_import::is_import(&input)
        {
            return Err(forbidden(
                "Actor archival requires a direct Owner request to Core",
            ));
        }
        if !self.account_actor_ids(&core.id)?.contains(target) {
            return Err(forbidden("Actor belongs to another account"));
        }
        self.store().set_archived(target, archived)
    }

    pub(crate) fn appeal_from_actor(
        &self,
        caller: &ActorCallContext,
        approval_id: &str,
        explanation: &str,
    ) -> Result<ApprovalRecord> {
        let actor = self.record_for_tool(&caller.actor_id, caller)?;
        if actor.remote_read_only
            || !self
                .account_actor_ids(&actor.id)?
                .contains(crate::stage::shared::ACTOR_ID)
        {
            return Err(forbidden("Appeal is outside the Actor's account authority"));
        }
        let record = self.store().appeal_approval(
            &actor.id,
            &caller.message_id,
            approval_id,
            &self.safe_error(explanation),
            self.store()
                .public_stage_id()
                .map_err(|error| miette::miette!("{error}"))?,
            false,
        )?;
        self.wake(crate::stage::shared::ACTOR_ID);
        if let Some(core) = self.shared_core() {
            core.notify();
        }
        Ok(record)
    }

    pub(crate) async fn resolve_appeal_from_core(
        &self,
        caller: &ActorCallContext,
        approval_id: &str,
        decision: ApprovalDecision,
        reason: &str,
    ) -> Result<ApprovalRecord> {
        let record = self.core_approval_appeal(approval_id)?;
        let authorization = self.prepare_appeal_authorization(caller, &record, decision, reason)?;
        self.apply_appeal_authorization(&authorization).await
    }

    pub(crate) fn core_approval_appeal(&self, approval_id: &str) -> Result<ApprovalRecord> {
        let record = self.store().approval(approval_id)?;
        if !self
            .account_actor_ids(crate::stage::shared::ACTOR_ID)?
            .contains(&record.source_actor(self.store())?)
            || !self.store().native_approval(approval_id)?
        {
            return Err(forbidden("Appeal does not belong to this account device"));
        }
        Ok(record)
    }

    pub(crate) fn prepare_appeal_authorization(
        &self,
        caller: &ActorCallContext,
        record: &ApprovalRecord,
        decision: ApprovalDecision,
        reason: &str,
    ) -> Result<ApprovalAppealAuthorization> {
        let core = self.record_for_tool(crate::stage::shared::ACTOR_ID, caller)?;
        let input = self.store().message(&caller.message_id)?;
        if core.remote_read_only
            || (!self.local_core() && !self.core_execution_permitted())
            || input.remote_authority.is_some()
            || crate::conversation_import::is_import(&input)
        {
            return Err(forbidden("Current Core authority cannot resolve appeals"));
        }
        let appeal = record
            .appeal
            .as_ref()
            .ok_or_else(|| invalid("Approval has no appeal"))?;
        Ok(ApprovalAppealAuthorization {
            approval_id: record.id.clone(),
            core_message_id: appeal.core_message_id.clone(),
            decision_message_id: caller.message_id.clone(),
            owner_message_id: (input.source == MessageSource::User).then_some(input.id),
            scopes_digest: super::approval::scopes_digest(record)?,
            decision,
            reason: self.safe_error(reason),
        })
    }

    /// Invoked only by the authenticated current Core runtime, never a model tool
    /// on this device. Applying a grant does not construct or run a Core worker.
    pub(crate) async fn apply_appeal_authorization(
        &self,
        authorization: &ApprovalAppealAuthorization,
    ) -> Result<ApprovalRecord> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let record = self.core_approval_appeal(&authorization.approval_id)?;
        let decision = authorization.decision;
        let original = self.store().message(&record.message_id)?;
        let actor = self.execution_actor(&original)?;
        if actor.remote_read_only {
            return Err(forbidden("Appeal permission mode or authority changed"));
        }
        if decision == ApprovalDecision::Approved {
            // An appeal intentionally outlives its inbox input. Build the current
            // hard policy only; never resurrect that input as tool authority.
            let base =
                self.build_app_context(actor.clone(), &original.id, original.seq as u64, None)?;
            if record.requested.clone().canonicalized()? != record.requested {
                return Err(forbidden(
                    "Appeal filesystem targets changed; submit a fresh operation",
                ));
            }
            for (roots, denied) in [
                (
                    &record.requested.readable_roots,
                    &base.sandbox_policy.filesystem.deny_read_paths,
                ),
                (
                    &record.requested.writable_roots,
                    &base.sandbox_policy.filesystem.deny_write_paths,
                ),
            ] {
                if roots
                    .iter()
                    .any(|path| denied.iter().any(|deny| path.starts_with(deny)))
                {
                    return Err(forbidden(
                        "Appeal scope conflicts with the current sandbox policy",
                    ));
                }
            }
        }
        let record = self.store().resolve_approval_appeal(authorization)?;
        self.wake(&record.source_actor(self.store())?);
        if record.source_actor(self.store())? == crate::stage::shared::ACTOR_ID
            && let Some(core) = self.shared_core()
        {
            core.notify();
        }
        Ok(record)
    }

    pub(crate) fn appeal_from_current_core(
        &self,
        approval_id: &str,
        explanation: &str,
    ) -> Result<ApprovalRecord> {
        let record = self.core_approval_appeal(approval_id)?;
        if record.source_actor(self.store())? != crate::stage::shared::ACTOR_ID {
            return Err(forbidden(
                "Core cannot appeal another Actor's denial on its behalf",
            ));
        }
        let record = self.store().appeal_approval(
            crate::stage::shared::ACTOR_ID,
            &record.message_id,
            approval_id,
            &self.safe_error(explanation),
            self.store()
                .public_stage_id()
                .map_err(|error| miette::miette!("{error}"))?,
            true,
        )?;
        if let Some(core) = self.shared_core() {
            core.notify();
        }
        Ok(record)
    }
    pub fn list_for_actor(&self, sender: &str) -> Result<Vec<ActorRecord>> {
        self.actor(sender)?;
        let allowed = self.account_actor_ids(sender)?;
        let input = self.store().current_message(sender)?;
        let remote = input
            .as_ref()
            .and_then(|input| input.remote_authority.as_ref())
            .filter(|authority| authority.reply_for.is_none());
        Ok(self
            .store()
            .actors()?
            .into_iter()
            .filter(|actor| {
                allowed.contains(&actor.id)
                    && remote.is_none_or(|authority| {
                        self.remote_device_authority(authority, &actor.id)
                            .is_ok_and(|grant| grant.read)
                    })
            })
            .collect())
    }
    fn remote_device_authority(
        &self,
        authority: &RemoteAuthority,
        actor: &str,
    ) -> Result<crate::stage::ledger::DeviceAuthority> {
        let local = self
            .store()
            .stage_local_id()
            .map_err(|_| forbidden("Remote device authority is unavailable"))?;
        crate::stage::ledger::StageLedger::attach(self.store(), local)
            .authorize_actor(authority.peer, actor, now())
            .map_err(|_| forbidden("Remote Actor authority is unavailable"))
    }
    pub(crate) fn ensure_delegation_authority(&self, input: &MessageRecord) -> Result<()> {
        if let Some(authority) = &input.remote_authority
            && authority.reply_for.is_none()
        {
            let grant = self.remote_device_authority(authority, &authority.origin_actor)?;
            if !grant.delegate {
                return Err(forbidden("Remote authority cannot delegate work"));
            }
        }
        Ok(())
    }
    fn account_actor_ids(&self, sender: &str) -> Result<std::collections::HashSet<String>> {
        self.store().account_actor_ids(sender)
    }
    pub fn record_for_tool(&self, id: &str, call: &ActorCallContext) -> Result<ActorRecord> {
        if call.actor_id != id {
            return Err(forbidden("Actor identity mismatch"));
        }
        let input = self.store().message(&call.message_id)?;
        if input.actor_id != id || input.status != MessageStatus::Running || input.cancel_requested
        {
            return Err(forbidden("Tool request authority is no longer valid"));
        }
        let mut actor = self.execution_actor(&input)?;
        actor.remote_read_only |= call.remote_read_only;
        if let Some(policy) = &call.device_policy {
            if id != crate::stage::shared::ACTOR_ID {
                return Err(forbidden("Device App policy belongs to the shared Actor"));
            }
            let actor = self.core_device_actor()?;
            let mut actor = actor;
            actor.remote_read_only |= call.remote_read_only;
            actor.config = policy.clone();
            return Ok(actor);
        }
        Ok(actor)
    }
    pub fn delegated_request(&self, sender: &str, id: &str) -> Result<MessageRecord> {
        let message = self.store().message(id)?;
        if !matches!(&message.source, MessageSource::Actor { actor_id } if actor_id == sender)
            || !matches!(&message.reply_to, ReplyTarget::Actor { actor_id, .. } if actor_id == sender)
            || !matches!(message.kind, MessageKind::Request)
        {
            return Err(forbidden("Request was not delegated by this Actor"));
        }
        Ok(message)
    }
    pub fn deliver_from_actor(
        &self,
        sender: &str,
        input: &str,
        call: &str,
        content: &str,
    ) -> Result<MessageRecord> {
        if sender == crate::stage::shared::ACTOR_ID && !self.core_execution_permitted() {
            return Err(forbidden("Core worker no longer owns execution"));
        }
        let delivery = self.store().deliver(sender, input, call, content)?;
        if delivery.status == MessageStatus::Queued {
            self.wake(&delivery.actor_id);
        }
        Ok(delivery)
    }
    pub fn message_from_actor(
        &self,
        sender: &str,
        current_id: &str,
        target: &str,
        content: String,
    ) -> Result<MessageRecord> {
        self.message_from_actor_with_context(sender, current_id, target, content, None, None)
    }
    pub(crate) fn message_from_actor_with_context(
        &self,
        sender: &str,
        current_id: &str,
        target: &str,
        content: String,
        context: Option<&AppToolExecutionContext>,
        related: Option<&str>,
    ) -> Result<MessageRecord> {
        if self.inner.reconfiguring.load(Ordering::Acquire)
            || self.inner.stopping.load(Ordering::Acquire)
        {
            return Err(ActorError::Unavailable("Actor configuration is changing".into()).into());
        }
        let current = self.store().message(current_id)?;
        if current.actor_id != sender
            || current.status != MessageStatus::Running
            || current.cancel_requested
        {
            return Err(forbidden("Actor message authority is not valid"));
        }
        self.ensure_delegation_authority(&current)?;
        let mut source = if sender == crate::stage::shared::ACTOR_ID {
            self.core_device_actor()?
        } else {
            self.execution_actor(&current)?
        };
        if let Some(context) = context {
            source = self.record_for_tool(
                sender,
                context
                    .actor_call
                    .as_ref()
                    .ok_or_else(|| forbidden("Missing delegation authority"))?,
            )?;
        }
        let _recipient = self.actor(target)?;
        if !self.account_actor_ids(sender)?.contains(target) {
            return Err(forbidden("Recipient belongs to another account"));
        }
        if content.is_empty() || content.len() > 64 * 1024 {
            return Err(invalid("Invalid Actor message size"));
        }
        if sender == target {
            return Err(invalid("Self-delegation is not allowed"));
        }
        let mut message = self.make_message(
            target,
            MessageSource::Actor {
                actor_id: sender.into(),
            },
            ReplyTarget::Actor {
                actor_id: sender.into(),
                request_id: current_id.into(),
            },
            content,
            vec![],
        );
        if let Some(related) = related {
            let original = self.store().message(related)?;
            let received_from_target = original.actor_id == sender
                && matches!(&original.source, MessageSource::Actor { actor_id } if actor_id == target);
            if original.actor_id != target && !received_from_target {
                return Err(forbidden("Related message is outside this conversation"));
            }
            message.in_reply_to = Some(original.id);
        }
        if current.remote_authority.is_some() {
            message.remote_authority = current.remote_authority.clone();
            if let Some(authority) = &mut message.remote_authority {
                authority.read_only |= source.remote_read_only;
            }
        }
        let message = self.store().enqueue(message, None)?;
        self.wake(target);
        Ok(message)
    }
    fn make_message(
        &self,
        target: &str,
        source: MessageSource,
        reply_to: ReplyTarget,
        content: String,
        attachments: Vec<Attachment>,
    ) -> MessageRecord {
        MessageRecord {
            seq: 0,
            id: Uuid::new_v4().to_string(),
            actor_id: target.into(),
            source,
            reply_to,
            kind: MessageKind::Request,
            in_reply_to: None,

            remote_authority: None,
            content,
            attachments,
            status: MessageStatus::Queued,
            cancel_requested: false,
            final_generation: None,
            started_at_ms: None,
            finished_at_ms: None,
            reply: None,
            error: None,
            created_at_ms: now(),
            updated_at_ms: now(),
        }
    }
    pub fn send_user(
        &self,
        target: &str,
        content: String,
        attachments: Vec<Attachment>,
        idempotency_key: Option<String>,
    ) -> Result<MessageRecord> {
        if self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
            || target != crate::stage::shared::ACTOR_ID && self.config().validate().is_err()
        {
            return Err(ActorError::Unavailable("Agent configuration is not ready".into()).into());
        }
        if content.len() > 64 * 1024 || content.trim().is_empty() && attachments.is_empty() {
            return Err(invalid("Invalid message or attachment count"));
        }
        if idempotency_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || key.len() > 200)
        {
            return Err(invalid("Invalid idempotency key"));
        }
        self.actor(target)?;
        if attachments
            .iter()
            .filter(|a| a.media_type.starts_with("image/"))
            .map(|a| a.size_bytes)
            .sum::<u64>()
            > 16 * 1024 * 1024
        {
            return Err(invalid("Image attachments exceed the message budget"));
        }
        for attachment in &attachments {
            let resource = self.store().resource(target, &attachment.id)?;
            if resource.uri != attachment.uri
                || resource.name != attachment.name
                || resource.media_type != attachment.media_type
                || resource.size_bytes != attachment.size_bytes
            {
                return Err(forbidden("Attachment metadata does not match its Actor"));
            }
        }
        let message = self.make_message(
            target,
            MessageSource::User,
            ReplyTarget::User,
            content,
            attachments,
        );
        let result = self.store().enqueue(message, idempotency_key.as_deref())?;
        self.wake(target);
        if target == crate::stage::shared::ACTOR_ID
            && let Some(core) = self.shared_core()
        {
            core.notify();
        }
        Ok(result)
    }
    pub fn cancel(&self, actor_id: &str, message_id: &str) -> Result<()> {
        let message = self.store().message(message_id)?;
        if message.actor_id != actor_id {
            return Err(forbidden("Message belongs to another Actor"));
        }
        let message = self.store().request_cancel(message_id)?;
        if message.status == MessageStatus::Queued
            && let Some(result) = self.store().finish_message(
                message_id,
                MessageStatus::Cancelled,
                None,
                Some("Request cancelled"),
            )?
        {
            self.wake(&result.actor_id);
        }
        if message.status == MessageStatus::Running
            && let Some(worker) = self.inner.workers.lock().get(actor_id)
        {
            worker.control.cancelled.lock().insert(message_id.into());
            worker.control.wake.notify_one();
        }
        Ok(())
    }
    pub(crate) async fn stop_from_core(
        &self,
        target: &str,
        caller: &ActorCallContext,
    ) -> Result<usize> {
        self.validate_actor_stop(target, caller)?;
        let system = self.clone();
        let target = target.to_owned();
        let caller = caller.clone();
        struct CancelAdmission(Arc<AtomicBool>);
        impl Drop for CancelAdmission {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let cancelled = Arc::new(AtomicBool::new(false));
        let _admission = CancelAdmission(cancelled.clone());
        // Cleanup survives cancellation of Core's future once the stop is committed.
        // Keep the old worker registered until its Apps release running effects.
        tokio::spawn(async move {
            let _lifecycle = system.inner.lifecycle.lock().await;
            if cancelled.load(Ordering::Acquire) {
                return Err(forbidden("Actor stop was cancelled before admission"));
            }
            system.validate_actor_stop(&target, &caller)?;
            let (ids, retiring) = {
                let workers = system.inner.workers.lock();
                let ids = system.store().request_cancel_actor(&target)?;
                let retiring = workers.get(&target).map(|worker| {
                    worker.control.stop.store(true, Ordering::Release);
                    worker.control.wake.notify_one();
                    worker.control.clone()
                });
                (ids, retiring)
            };
            for id in &ids {
                if system.store().message(id)?.status == MessageStatus::Queued {
                    system
                        .store()
                        .settle_cancelled_inputs(std::slice::from_ref(id))?;
                }
            }
            if let Some(retiring) = retiring {
                let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
                loop {
                    let finished = {
                        let workers = system.inner.workers.lock();
                        workers.get(&target).is_none_or(|worker| {
                            !Arc::ptr_eq(&worker.control, &retiring) || worker.task.is_finished()
                        })
                    };
                    if finished || tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                let remaining = {
                    let mut workers = system.inner.workers.lock();
                    if workers
                        .get(&target)
                        .is_some_and(|worker| Arc::ptr_eq(&worker.control, &retiring))
                    {
                        workers.remove(&target)
                    } else {
                        None
                    }
                };
                if let Some(worker) = remaining {
                    worker.task.abort();
                    let _ = worker.task.await;
                }
            }
            system.store().settle_cancelled_inputs(&ids)?;
            system.start_queued_workers()?;
            Ok(ids.len())
        })
        .await
        .map_err(|error| miette::miette!("Actor stop cleanup failed: {error}"))?
    }

    fn validate_actor_stop(&self, target: &str, caller: &ActorCallContext) -> Result<()> {
        let core = self.record_for_tool(crate::stage::shared::ACTOR_ID, caller)?;
        if core.remote_read_only
            || crate::conversation_import::is_import(&self.store().message(&caller.message_id)?)
            || self
                .store()
                .message(&caller.message_id)?
                .remote_authority
                .is_some()
            || target == crate::stage::shared::ACTOR_ID
            || !self.account_actor_ids(&core.id)?.contains(target)
        {
            return Err(forbidden(
                "Stopping an Actor requires account Core authority over a hosted Actor",
            ));
        }
        Ok(())
    }

    pub async fn update_owned(&self, id: &str, config: ActorConfig) -> Result<ActorRecord> {
        if id == crate::stage::shared::ACTOR_ID && self.shared_core().is_some() {
            return Err(forbidden("Shared Actor only accepts model preferences"));
        }
        self.update_owned_with(id, |_| normalize_config(config))
            .await
    }
    /// Apply conversation model preferences under the lifecycle lock.
    pub async fn patch_owned_preferences(
        &self,
        id: &str,
        model: Option<Option<String>>,
    ) -> Result<ActorRecord> {
        if id == crate::stage::shared::ACTOR_ID
            && let Some(core) = self.shared_core()
        {
            core.preferences(model)
                .await
                .map_err(|error| miette::miette!("{error}"))?;
            return self.actor(id);
        }
        self.apply_owned_preferences(id, model).await
    }
    pub(crate) async fn apply_owned_preferences(
        &self,
        id: &str,
        model: Option<Option<String>>,
    ) -> Result<ActorRecord> {
        self.update_owned_with(id, |mut config| {
            if let Some(model) = model {
                config.model = model;
            }
            let checked = normalize_config(config.clone())?;
            if checked.workspace != config.workspace {
                return Err(forbidden(
                    "Preference changes cannot rebind filesystem grants",
                ));
            }
            Ok(config)
        })
        .await
    }
    async fn update_owned_with(
        &self,
        id: &str,
        update: impl FnOnce(ActorConfig) -> Result<ActorConfig>,
    ) -> Result<ActorRecord> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let mut actor = self.actor(id)?;
        let config = update(actor.config.clone())?;
        let config = if id == crate::stage::shared::ACTOR_ID {
            config
        } else {
            self.hosted_config(config)?
        };
        self.validate_config(&config)?;
        // All fallible preparation precedes interruption. A bad path must not
        // remove the live worker or leave a previously valid Actor stranded.
        crate::file_io::ensure_directory(
            &crate::sandbox::RuntimeSandboxPolicy::disabled(),
            &config.workspace,
        )?;
        let pause = Reconfiguration::begin(&self.inner.reconfiguring, &self.inner.metadata);
        let affected = std::collections::HashSet::from([id.to_owned()]);
        let workers: Vec<_> = {
            let mut workers = self.inner.workers.lock();
            affected
                .iter()
                .filter_map(|id| workers.remove(id).map(|worker| (id.clone(), worker)))
                .collect()
        };
        for (_, worker) in &workers {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
        }
        for (id, worker) in workers {
            self.stop_actor_worker(&id, worker).await?;
        }
        actor.config = config;
        if actor.status != ActorStatus::Archived {
            actor.status = ActorStatus::Idle;
        }
        actor.updated_at_ms = now();
        let saved = self.store().save_actor(&actor);
        drop(pause);
        self.start_queued_workers()?;
        saved?;
        Ok(actor)
    }
    pub fn validate_config_update(&self, config: &Config) -> Result<()> {
        config
            .validate_settings()
            .map_err(|_| invalid("Provider configuration is not ready"))?;
        Ok(())
    }
    pub async fn update_config(&self, config: Config) -> Result<()> {
        self.update_config_with(config, std::future::ready(Ok(())))
            .await
    }
    /// Validate the entire Actor registry before writing durable configuration.
    /// Old execution is stopped before credentials/policy are replaced; a failed
    /// write keeps the old configuration and resumes queued work without replay.
    pub async fn update_config_with(
        &self,
        config: Config,
        persist: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let pause = Reconfiguration::begin(&self.inner.reconfiguring, &self.inner.metadata);
        self.validate_config_update(&config)?;
        let removed_models = self
            .store()
            .actor_models()?
            .into_iter()
            .filter(|name| !config.models.contains_key(name))
            .collect::<Vec<_>>();
        let workers: Vec<_> = self.inner.workers.lock().drain().collect();
        for (_, worker) in &workers {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
        }
        for (id, worker) in workers {
            self.stop_actor_worker(&id, worker).await?;
        }
        let persisted = persist.await;
        if persisted.is_ok() {
            self.store().reset_model_preferences(&removed_models)?;
            *self.inner.config.write() = config;
        }
        drop(pause);
        self.start_queued_workers()?;
        persisted
    }
    async fn stop_actor_worker(&self, id: &str, worker: Worker) -> Result<()> {
        worker.control.stop.store(true, Ordering::Release);
        worker.control.wake.notify_one();
        stop_worker(worker).await;
        // Includes a panic/forced stop during App shutdown or synchronous I/O.
        // Repair only this inbox, never touch other Actors that may be running.
        self.store()
            .interrupt_actor(id, "Execution stopped during Actor reconfiguration")?;
        // A forced-stop repair can enqueue a correlated result to the parent.
        // Wake workers after the transaction just as normal completion does.
        for worker in self.inner.workers.lock().values() {
            worker.control.wake.notify_one();
        }
        Ok(())
    }
    pub(crate) async fn quiesce_core(&self) -> Result<()> {
        let _lifecycle = self.inner.lifecycle.lock().await;
        let worker = self
            .inner
            .workers
            .lock()
            .remove(crate::stage::shared::ACTOR_ID);
        if let Some(worker) = worker {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
            stop_worker(worker).await;
        }
        self.store()
            .set_status(crate::stage::shared::ACTOR_ID, ActorStatus::Idle)?;
        Ok(())
    }
    pub(crate) fn is_stopping(&self) -> bool {
        self.inner.stopping.load(Ordering::Acquire)
    }
    pub async fn shutdown(&self) {
        let _lifecycle = self.inner.lifecycle.lock().await;
        self.inner.stopping.store(true, Ordering::Release);
        self.store().changed().notify_waiters();
        let workers: Vec<_> = self.inner.workers.lock().drain().collect();
        for (_, worker) in &workers {
            worker.control.stop.store(true, Ordering::Release);
            worker.control.wake.notify_one();
        }
        for (id, worker) in workers {
            let _ = self.stop_actor_worker(&id, worker).await;
        }
        if let Some(host) = self.inner.sudo_host.lock().await.take() {
            host.close();
        }
        let temporary = self.inner.temporary.lock().take();
        drop(temporary);
    }
    pub fn execution_context(
        &self,
        actor: &ActorRecord,
        message: &MessageRecord,
    ) -> Result<AppToolExecutionContext> {
        self.execution_context_with_policy(actor, message, None)
    }
    pub(crate) fn execution_context_with_policy(
        &self,
        actor: &ActorRecord,
        message: &MessageRecord,
        device_policy: Option<ActorConfig>,
    ) -> Result<AppToolExecutionContext> {
        let actor = self.record_for_tool(
            &actor.id,
            &ActorCallContext {
                remote_read_only: actor.remote_read_only,
                actor_id: actor.id.clone(),
                message_id: message.id.clone(),
                device_policy: device_policy.clone(),
            },
        )?;
        self.build_app_context(actor, &message.id, message.seq as u64, device_policy)
    }
    pub(crate) fn owner_app_context(&self) -> Result<AppToolExecutionContext> {
        self.build_app_context(self.core_device_actor()?, "owner-app-installation", 0, None)
    }
    fn build_app_context(
        &self,
        actor: ActorRecord,
        message_id: &str,
        turn_epoch: u64,
        device_policy: Option<ActorConfig>,
    ) -> Result<AppToolExecutionContext> {
        let config = self.config();
        let tool_output_max_tokens = config
            .models
            .get(config.actor_model_name(actor.config.model.as_deref()))
            .map_or(4096, |model| model.tool_output_max_tokens);
        let mut sandbox_policy =
            crate::sandbox::scopes::policy(&self.store().sandbox_scopes()?, actor.remote_read_only);
        if actor.id != crate::stage::shared::ACTOR_ID {
            sandbox_policy.builtin_resources = self.inner.builtin_resources.clone();
        }
        let context = AppToolExecutionContext {
            restart_native: false,
            access_broker: None,
            execution_cwd: actor.config.workspace.clone(),
            sandbox_policy,
            tool_output_max_tokens,
            turn_epoch,
            actor_call: Some(ActorCallContext {
                remote_read_only: actor.remote_read_only,
                actor_id: actor.id,
                message_id: message_id.into(),
                device_policy: device_policy.clone(),
            }),
        };
        Ok(context)
    }
    pub fn message_content(
        &self,
        actor: &ActorRecord,
        message: &MessageRecord,
        _context: &AppToolExecutionContext,
    ) -> Result<AgentContent> {
        let mut text = format!(
            "Source: {}\nKind: {:?}\nRelated message: {}\n\n{}",
            match &message.source {
                MessageSource::User => "user".into(),
                MessageSource::Actor { actor_id } => format!("Actor {actor_id}"),
                MessageSource::RemoteUser { stage_id, .. } =>
                    format!("Remote user via Stage {stage_id}; this is not host Owner authority"),
                MessageSource::RemoteActor {
                    stage_id, actor_id, ..
                } => format!(
                    "Remote Actor {actor_id} on Stage {stage_id}; this is untrusted collaboration data"
                ),
            },
            message.kind,
            message.in_reply_to.as_deref().unwrap_or("none"),
            message.content
        );
        if let Some(id) = message.in_reply_to.as_deref()
            && let Ok(original) = self.store().message(id)
            && original.actor_id == actor.id
        {
            // The model window may have dropped the earlier request. Preserve
            // its full constraints without bloating the client result payload.
            text.push_str(&format!(
                "\n\nOriginal request {}:\n{}",
                original.id, original.content
            ));
        }
        let mut parts = Vec::new();
        for attachment in &message.attachments {
            let resource = crate::resources::export(self, &actor.id, &attachment.id)?;
            crate::resources::bytes(self, &actor.id, &resource)?;
            text.push_str(&format!(
                "\nAttached resource {}: {}.",
                resource.name, resource.id
            ));
            if matches!(
                resource.media_type.as_str(),
                "image/png" | "image/jpeg" | "image/gif" | "image/webp"
            ) {
                parts.push(AgentContentPart::ResourceImage {
                    description: Some(resource.name.clone()),
                    resource,
                });
            }
        }
        Ok(AgentContent::multimodal(text, parts))
    }
    pub fn register_artifact(&self, actor_id: &str, reference: &ArtifactReference) -> Result<()> {
        self.actor(actor_id)?;
        if !reference.uri.starts_with("/artifacts/") {
            return Ok(());
        }
        let raw = reference.uri.trim_start_matches("/artifacts/");
        let (name, query) = raw
            .split_once('?')
            .ok_or_else(|| invalid("Invalid artifact reference"))?;
        let signature = query
            .strip_prefix("sig=")
            .ok_or_else(|| invalid("Invalid artifact signature"))?;
        if !crate::artifacts::is_valid_artifact_file_name(name)
            || !crate::artifacts::verify_artifact_signature(
                &crate::artifacts::artifact_signing_secret(),
                name,
                signature,
            )
        {
            return Err(forbidden("Invalid artifact reference"));
        }
        let source = daat_locus_paths_sync().artifact_dir("presented").join(name);
        let destination = self.actor_data_dir(actor_id).join("artifacts").join(name);
        std::fs::create_dir_all(destination.parent().unwrap()).into_diagnostic()?;
        std::fs::copy(source, &destination).into_diagnostic()?;
        protect_private_path(&destination)?;
        let size_bytes = std::fs::metadata(&destination).into_diagnostic()?.len();
        let current = self.store().current_message(actor_id)?;
        self.store().register_resource_for_message(
            StoredResource {
                id: name.into(),
                actor_id: actor_id.into(),
                path: destination,
                name: reference.title.clone(),
                media_type: reference.mime_type.clone(),
                size_bytes,
                uri: format!("/v1/actors/{actor_id}/resources/{name}"),
            },
            current.as_ref().map(|m| m.id.as_str()),
        )
    }
    pub fn safe_error(&self, detail: &str) -> String {
        self.redact_secrets(detail).chars().take(2000).collect()
    }
    pub fn redact_secrets(&self, detail: &str) -> String {
        let mut message = detail.to_owned();
        for provider in self.config().providers.values() {
            match provider {
                ProviderConfig::Openai { api_key, .. }
                | ProviderConfig::AnthropicCompatible { api_key, .. }
                | ProviderConfig::OpenaiCompatible { api_key, .. } => {
                    message = redact_secret_text(&message, &resolve_env_reference(api_key))
                }
                ProviderConfig::GithubCopilot { github_token } => {
                    message = redact_secret_text(&message, &resolve_env_reference(github_token))
                }
                ProviderConfig::Ollama {
                    api_key: Some(key), ..
                } => message = redact_secret_text(&message, &resolve_env_reference(key)),
                ProviderConfig::OpenaiCodexOauth { auth_file, .. }
                | ProviderConfig::OpenCodeConsoleOauth { auth_file, .. }
                | ProviderConfig::OpenCodeGoOauth { auth_file, .. } => {
                    fn scrub(value: &serde_json::Value, message: &mut String) {
                        if let Some(fields) = value.as_object() {
                            for (key, value) in fields {
                                if matches!(
                                    key.as_str(),
                                    "access_token" | "refresh_token" | "id_token" | "token"
                                ) {
                                    if let Some(secret) = value.as_str() {
                                        *message = redact_secret_text(message, secret);
                                    }
                                } else {
                                    scrub(value, message);
                                }
                            }
                        }
                    }
                    if let Ok(bytes) = crate::file_io::read_authorized_bytes(
                        std::path::Path::new(&resolve_env_reference(auth_file)),
                        128 * 1024,
                        &crate::sandbox::RuntimeSandboxPolicy::disabled(),
                    ) && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes)
                    {
                        scrub(&value, &mut message);
                    }
                }
                _ => {}
            }
        }
        message
    }
}
/// Gate short synchronous metadata mutations while a lifecycle operation awaits
/// old execution. The lock itself is never held across an await.
struct Reconfiguration<'a>(&'a AtomicBool);
impl<'a> Reconfiguration<'a> {
    fn begin(flag: &'a AtomicBool, metadata: &Mutex<()>) -> Self {
        let _metadata = metadata.lock();
        flag.store(true, Ordering::Release);
        Self(flag)
    }
}
impl Drop for Reconfiguration<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
async fn stop_worker(mut worker: Worker) {
    if tokio::time::timeout(std::time::Duration::from_secs(5), &mut worker.task)
        .await
        .is_err()
    {
        worker.task.abort();
        let _ = worker.task.await;
    }
}

#[cfg(test)]
mod tests {
    const FIXTURE_ACTOR_ID: &str = "root";
    use super::*;

    #[tokio::test]
    async fn committed_actor_stop_finishes_cleanup_even_when_its_core_call_is_cancelled() {
        struct OwnedResource(Arc<AtomicBool>);
        impl Drop for OwnedResource {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let (_directory, system) = fixture().await;
        system.install_local_core().unwrap();
        let first = system
            .send_user("root", "Original input".into(), vec![], None)
            .unwrap();
        system.store().claim_next("root").unwrap().unwrap();
        let second = system
            .send_user("root", "Nested input".into(), vec![], None)
            .unwrap();
        system
            .store()
            .claim_incoming("root", &first.id)
            .unwrap()
            .unwrap();
        system
            .send_user("core", "Stop the Actor".into(), vec![], None)
            .unwrap();
        let input = system.store().claim_next("core").unwrap().unwrap();
        let caller = system
            .execution_context(&system.actor("core").unwrap(), &input)
            .unwrap()
            .actor_call
            .unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let resource = OwnedResource(released.clone());
        let task = tokio::spawn(async move {
            let _resource = resource;
            std::future::pending::<()>().await;
        });
        system.inner.workers.lock().insert(
            "root".into(),
            Worker {
                control: Arc::new(ActorControl::default()),
                task,
            },
        );
        let stopping = system.clone();
        let call = tokio::spawn(async move { stopping.stop_from_core("root", &caller).await });
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while !system.store().message(&first.id).unwrap().cancel_requested {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        call.abort();
        let _ = call.await;
        tokio::time::timeout(std::time::Duration::from_secs(8), async {
            while system.store().message(&first.id).unwrap().status != MessageStatus::Cancelled
                || system.store().message(&second.id).unwrap().status != MessageStatus::Cancelled
            {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("committed stop lost cleanup after its caller was cancelled");
        assert!(released.load(Ordering::Acquire));
        assert!(system.store().current_message("root").unwrap().is_none());
        system.shutdown().await;
    }

    struct NoModels;
    impl ModelFactory for NoModels {
        fn build(&self, _: &str, _: &Config) -> Result<Arc<dyn ModelProvider>> {
            Err(invalid("A paused test must never invoke a provider"))
        }
    }
    async fn fixture() -> (tempfile::TempDir, ActorSystem) {
        let directory = crate::sandbox::test_support::scoped_directory();
        let mut config = Config {
            main_model: "mock".into(),
            ..Default::default()
        };
        config.models.insert(
            "mock".into(),
            crate::config::ModelConfig {
                provider: "mock".into(),
                model_id: "mock".into(),
                ..Default::default()
            },
        );
        config.providers.insert(
            "mock".into(),
            ProviderConfig::OpenaiCompatible {
                base_url: "https://example.invalid".into(),
                api_key: "unused-test-key".into(),
            },
        );
        let system = ActorSystem::open_paused_with_factory(
            directory.path().join("data"),
            directory.path().join("workspace"),
            config,
            Arc::new(NoModels),
        )
        .await
        .unwrap();
        system.seed_test_actor(FIXTURE_ACTOR_ID);
        (directory, system)
    }

    #[tokio::test]
    async fn actor_archive_retains_history_across_recovery_and_unarchive() {
        use crate::{
            app::AppManager,
            model_protocol::{AgentMessage, AgentToolCall},
        };
        let (directory, system) = fixture().await;
        let previous = system
            .send_user("root", "Keep this conversation".into(), vec![], None)
            .unwrap();
        system
            .store()
            .append_history(
                "root",
                &previous.id,
                &[AgentMessage::user("Keep this conversation")],
            )
            .unwrap();
        system
            .store()
            .finish_message(
                &previous.id,
                MessageStatus::Completed,
                Some("Retained answer"),
                None,
            )
            .unwrap();
        let before = system.actor("root").unwrap();
        let history = system.store().history("root", 10).unwrap();
        let input = system
            .send_user(
                "core",
                "Archive root, then unarchive it when I request it".into(),
                vec![],
                None,
            )
            .unwrap();
        let mut input = system.store().claim_next("core").unwrap().unwrap_or(input);

        let mut apps = AppManager::new(vec![Box::new(crate::actor_app::ActorApp::new(
            "core".into(),
            system.clone(),
        ))])
        .unwrap();
        for name in ["archive", "unarchive"] {
            let core = system.actor("core").unwrap();
            let context = system.execution_context(&core, &input).unwrap();
            let call = AgentToolCall {
                id: format!("archive-lifecycle-{name}"),
                name: format!("actors__{name}"),
                arguments: serde_json::json!({"stage_id":null,"actor_id":"root"}),
            };
            apps.validate_tool_call(&call, &context).unwrap();
            let result = apps.execute_tool(&call, &context).await.unwrap();
            let archived = name == "archive";
            assert_eq!(
                result.payload["status"],
                if archived { "archived" } else { "idle" }
            );
            if archived {
                assert!(
                    system
                        .send_user("root", "Blocked".into(), vec![], None)
                        .is_err()
                );
                assert!(system.store().claim_next("root").unwrap().is_none());
                // Lifecycle cleanup and process recovery must not unarchive it.
                system.store().interrupt_actor("root", "Cleanup").unwrap();
                system.update_owned_with("root", Ok).await.unwrap();
                let reopened =
                    ActorStore::open(&directory.path().join("data/actors.sqlite3")).unwrap();
                reopened.recover().unwrap();
                assert_eq!(
                    reopened.actor("root").unwrap().status,
                    ActorStatus::Archived
                );
                // Recovery interrupts Core, so re-establish this test's next direct input.
                input = system
                    .send_user("core", "Unarchive root".into(), vec![], None)
                    .unwrap();
                input = system.store().claim_next("core").unwrap().unwrap_or(input);
            }
            let expected_config = before.config.clone();
            assert_eq!(system.actor("root").unwrap().config, expected_config);
            assert_eq!(system.actor("root").unwrap().context, before.context);
            assert!(system.store().history("root", 10).unwrap() == history);
            assert_eq!(
                system
                    .store()
                    .message(&previous.id)
                    .unwrap()
                    .reply
                    .as_deref(),
                Some("Retained answer")
            );
        }

        assert!(
            system
                .store()
                .approvals(Some("core"), None, 10)
                .unwrap()
                .items
                .is_empty()
        );
        let resumed = system
            .send_user("root", "New work".into(), vec![], None)
            .unwrap();
        assert_eq!(
            system.store().claim_next("root").unwrap().unwrap().id,
            resumed.id
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actor_archive_discards_queued_inputs_and_ignores_delegated_work() {
        let (_directory, system) = fixture().await;
        let core_input = system
            .send_user("core", "Archive root".into(), vec![], None)
            .unwrap();
        system.store().claim_next("core").unwrap().unwrap();
        let context = system
            .execution_context(&system.actor("core").unwrap(), &core_input)
            .unwrap();
        let caller = context.actor_call.as_ref().unwrap();
        let queued = system
            .send_user("root", "Work".into(), vec![], None)
            .unwrap();
        // Queued inputs no longer prevent archival; they are discarded instead.
        system
            .archive_from_core("root", true, caller)
            .await
            .unwrap();
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
        assert!(!system.store().has_queued_input("root").unwrap());
        assert!(
            system.store().message(&queued.id).is_err(),
            "queued inputs are discarded on archival"
        );
        system
            .archive_from_core("root", false, caller)
            .await
            .unwrap();
        assert!(
            system.store().claim_next("root").unwrap().is_none(),
            "a discarded input never runs after unarchive"
        );
        let input = system
            .send_user("root", "Work again".into(), vec![], None)
            .unwrap();
        system.store().claim_next("root").unwrap().unwrap();
        // A running input still does not prevent archival; it is left to finish.
        system
            .archive_from_core("root", true, caller)
            .await
            .unwrap();
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Running
        );
        system
            .archive_from_core("root", false, caller)
            .await
            .unwrap();
        system.seed_test_actor("child");
        let delegated = system
            .message_from_actor("root", &input.id, "child", "Research".into())
            .unwrap();
        system
            .store()
            .finish_message(&input.id, MessageStatus::Completed, Some("Delegated"), None)
            .unwrap();
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Idle);
        // Unfinished delegated work still does not block archival.
        system
            .archive_from_core("root", true, caller)
            .await
            .unwrap();
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
        system
            .archive_from_core("root", false, caller)
            .await
            .unwrap();
        system
            .store()
            .finish_message(
                &delegated.id,
                MessageStatus::Completed,
                Some("Found it"),
                None,
            )
            .unwrap();
        // The delivered result queues for root and the next archive discards it.
        assert!(system.store().has_queued_input("root").unwrap());
        system
            .archive_from_core("root", true, caller)
            .await
            .unwrap();
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
        assert!(!system.store().has_queued_input("root").unwrap());
        system
            .archive_from_core("root", false, caller)
            .await
            .unwrap();
        assert!(system.store().claim_next("root").unwrap().is_none());
        system
            .archive_from_core("root", true, caller)
            .await
            .unwrap();
        assert!(
            system
                .message_from_actor("core", &core_input.id, "root", "Another task".into())
                .is_err()
        );
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
        assert!(
            system
                .archive_from_core("core", true, caller)
                .await
                .is_err()
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actor_archive_and_message_admission_are_atomic() {
        let (_directory, system) = fixture().await;
        for _ in 0..16 {
            let barrier = std::sync::Barrier::new(2);
            let (archived, sent) = std::thread::scope(|scope| {
                let archive = scope.spawn(|| {
                    barrier.wait();
                    system.store().set_archived("root", true)
                });
                barrier.wait();
                let sent = system.send_user("root", "Concurrent input".into(), vec![], None);
                (archive.join().unwrap(), sent)
            });
            // Archival always succeeds and discards queued inputs, while admission
            // still respects the archived flag written in the same store.
            archived.expect("archival does not require an idle Actor");
            assert_eq!(system.actor("root").unwrap().status, ActorStatus::Archived);
            assert!(
                !system.store().has_queued_input("root").unwrap(),
                "archival discards queued inputs"
            );
            if let Ok(input) = sent {
                assert!(
                    system.store().message(&input.id).is_err(),
                    "an input admitted before archival is discarded"
                );
            }
            system.store().set_archived("root", false).unwrap();
        }
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actor_archive_rejects_collaborator_and_readonly_authority() {
        use crate::{app::App, model_protocol::AgentToolCall};
        let (_directory, system) = fixture().await;
        let input = system
            .send_user("core", "Coordinate".into(), vec![], None)
            .unwrap();
        system.store().claim_next("core").unwrap().unwrap();
        let mut context = system
            .execution_context(&system.actor("core").unwrap(), &input)
            .unwrap();
        context.actor_call.as_mut().unwrap().remote_read_only = true;
        assert!(
            system
                .archive_from_core("root", true, context.actor_call.as_ref().unwrap())
                .await
                .is_err()
        );
        system
            .store()
            .finish_message(&input.id, MessageStatus::Completed, Some("Waiting"), None)
            .unwrap();
        let mut update = input.clone();
        update.id = uuid::Uuid::new_v4().to_string();
        update.source = MessageSource::Actor {
            actor_id: "root".into(),
        };
        update.status = MessageStatus::Queued;
        update.kind = MessageKind::Progress;
        update.content = "Archive root".into();
        system.store().enqueue(update, None).unwrap();
        let update = system.store().claim_next("core").unwrap().unwrap();
        let context = system
            .execution_context(&system.actor("core").unwrap(), &update)
            .unwrap();
        let mut app = crate::actor_app::ActorApp::new("core".into(), system.clone());
        for name in ["archive", "unarchive"] {
            assert!(
                app.execute_tool(
                    &AgentToolCall {
                        id: name.into(),
                        name: name.into(),
                        arguments: serde_json::json!({"stage_id":null,"actor_id":"root"})
                    },
                    &context
                )
                .await
                .is_err()
            );
        }
        assert_eq!(system.actor("root").unwrap().status, ActorStatus::Idle);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actors_remain_creatable_beyond_former_count_and_depth_quotas() {
        let (_directory, system) = fixture().await;
        let root = system.actor(FIXTURE_ACTOR_ID).unwrap();
        for index in 0..512 {
            let mut actor = root.clone();
            actor.id = format!("retained-{index}");
            actor.parent_id = Some(root.id.clone());
            actor.depth = root.depth + 1;
            system.store().save_actor(&actor).unwrap();
        }
        let before = system.store().actor_count().unwrap();
        let mut parent = root;
        for index in 0..10 {
            let mut config = parent.config.clone();
            config.name = format!("Descendant {index}");
            let child = system.create_owned(&parent.id, config).unwrap();
            assert_eq!(child.parent_id.as_deref(), Some(parent.id.as_str()));
            assert_eq!(child.depth, parent.depth + 1);
            assert_eq!(child.status, ActorStatus::Idle);
            parent = child;
        }
        assert_eq!(system.store().actor_count().unwrap(), before + 10);
        assert_eq!(system.resident_workers(), 0);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn input_arriving_during_worker_retirement_is_not_lost() {
        let (_directory, system) = fixture().await;
        system.inner.paused.store(false, Ordering::Release);
        let retiring = Arc::new(ActorControl::default());
        system.inner.workers.lock().insert(
            FIXTURE_ACTOR_ID.into(),
            Worker {
                control: retiring.clone(),
                task: tokio::spawn(async {}),
            },
        );
        let input = system
            .send_user(FIXTURE_ACTOR_ID, "during cleanup".into(), vec![], None)
            .unwrap();
        assert!(Arc::ptr_eq(
            &system.inner.workers.lock()[FIXTURE_ACTOR_ID].control,
            &retiring
        ));
        system.worker_finished(FIXTURE_ACTOR_ID, &retiring, true);
        let replacement = system.inner.workers.lock()[FIXTURE_ACTOR_ID]
            .control
            .clone();
        assert!(!Arc::ptr_eq(&replacement, &retiring));
        // An old generation's completion must not remove the replacement.
        system.worker_finished(FIXTURE_ACTOR_ID, &retiring, true);
        assert!(Arc::ptr_eq(
            &system.inner.workers.lock()[FIXTURE_ACTOR_ID].control,
            &replacement
        ));
        system.suspend_execution().await.unwrap();
        system.worker_finished(FIXTURE_ACTOR_ID, &replacement, true);
        system.wake(FIXTURE_ACTOR_ID);
        assert_eq!(system.resident_workers(), 0);
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Queued
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn cancelling_queued_inputs_does_not_accumulate_worker_cancellation_state() {
        let (_directory, system) = fixture().await;
        let control = Arc::new(ActorControl::default());
        system.inner.workers.lock().insert(
            FIXTURE_ACTOR_ID.into(),
            Worker {
                control: control.clone(),
                task: tokio::spawn(async {}),
            },
        );
        for _ in 0..16 {
            let input = system
                .send_user(FIXTURE_ACTOR_ID, "queued".into(), vec![], None)
                .unwrap();
            system.cancel(FIXTURE_ACTOR_ID, &input.id).unwrap();
            assert_eq!(
                system.store().message(&input.id).unwrap().status,
                MessageStatus::Cancelled
            );
        }
        assert!(control.cancelled.lock().is_empty());
        system.shutdown().await;
    }

    #[tokio::test]
    async fn suspension_retains_inputs_and_account_activation_cannot_unpause_it() {
        let (_directory, system) = fixture().await;
        system.inner.account_gated.store(true, Ordering::Release);
        let input = system
            .send_user(FIXTURE_ACTOR_ID, "retained".into(), vec![], None)
            .unwrap();
        system.suspend_execution().await.unwrap();
        system.activate_account_workers().await.unwrap();
        assert!(!system.workers_enabled());
        assert_eq!(
            system.store().message(&input.id).unwrap().status,
            MessageStatus::Queued
        );
        assert!(system.inner.workers.lock().is_empty());
        system.resume_execution().await.unwrap();
        assert!(system.workers_enabled());
        system.shutdown().await;
    }

    #[tokio::test]
    async fn removed_actor_model_falls_back_after_successful_persistence() {
        let (_directory, system) = fixture().await;
        let mut config = system.config();
        config
            .models
            .insert("custom".into(), config.models["mock"].clone());
        system.update_config(config).await.unwrap();
        let mut actor = system.actor(FIXTURE_ACTOR_ID).unwrap().config;
        actor.model = Some("custom".into());
        actor.name = "Custom model worker".into();
        let child = system.create_owned(FIXTURE_ACTOR_ID, actor).unwrap();
        let mut candidate = system.config();
        candidate.models.remove("custom");
        let persisted = AtomicBool::new(false);
        system
            .update_config_with(candidate, async {
                persisted.store(true, Ordering::Release);
                Ok(())
            })
            .await
            .unwrap();
        assert!(persisted.load(Ordering::Acquire));
        assert!(!system.config().models.contains_key("custom"));
        assert!(system.actor(&child.id).unwrap().config.model.is_none());
        assert_eq!(system.config().main_model, "mock");
        assert!(!system.inner.reconfiguring.load(Ordering::Acquire));
        system.shutdown().await;
    }

    #[tokio::test]
    async fn removing_all_models_keeps_queued_input_without_starting_a_worker() {
        let (_directory, system) = fixture().await;
        let queued = system
            .send_user(
                FIXTURE_ACTOR_ID,
                "Keep the queued input".into(),
                vec![],
                None,
            )
            .unwrap();
        let mut empty = system.config();
        empty.providers.clear();
        empty.models.clear();
        empty.main_model.clear();
        empty.approval_model = None;
        system.update_config(empty).await.unwrap();
        system.resume_execution().await.unwrap();
        assert!(system.inner.workers.lock().is_empty());
        assert_eq!(
            system.store().message(&queued.id).unwrap().status,
            MessageStatus::Queued
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn failed_config_write_preserves_configuration_and_queued_work() {
        let (_directory, system) = fixture().await;
        let mut original = system.config();
        original
            .models
            .insert("custom".into(), original.models["mock"].clone());
        system.update_config(original).await.unwrap();
        let mut actor = system.actor(FIXTURE_ACTOR_ID).unwrap().config;
        actor.model = Some("custom".into());
        actor.name = "Custom model worker".into();
        let child = system.create_owned(FIXTURE_ACTOR_ID, actor).unwrap();
        let request = system
            .send_user(FIXTURE_ACTOR_ID, "queued work".into(), vec![], None)
            .unwrap();
        let mut candidate = system.config();
        candidate.models.remove("custom");
        candidate.approval_model = Some("mock".into());
        assert!(
            system
                .update_config_with(candidate, async {
                    Err(invalid("Simulated durable write failure"))
                })
                .await
                .is_err()
        );
        assert!(system.config().approval_model.is_none());
        assert!(system.config().models.contains_key("custom"));
        assert_eq!(
            system.actor(&child.id).unwrap().config.model.as_deref(),
            Some("custom")
        );
        assert_eq!(
            system.store().message(&request.id).unwrap().status,
            MessageStatus::Queued
        );
        assert!(
            system
                .send_user(FIXTURE_ACTOR_ID, "still available".into(), vec![], None)
                .is_ok()
        );
        system.shutdown().await;
    }

    #[tokio::test]
    async fn retired_capability_lists_are_removed_without_losing_history_or_project_access() {
        let (directory, system) = fixture().await;
        let actor = system.actor(FIXTURE_ACTOR_ID).unwrap();
        let queued = system
            .send_user(&actor.id, "Retain this request".into(), vec![], None)
            .unwrap();
        let history = crate::model_protocol::AgentMessage::user("Retain the original history");
        system
            .store()
            .append_history(&actor.id, &queued.id, std::slice::from_ref(&history))
            .unwrap();
        system.store().stage_transaction(|tx| {
            tx.execute("UPDATE actors SET record=json_set(record,'$.config.apps',json_array('files','study'),
                '$.config.permissions.app_access',json('[]'),'$.config.permissions.delegation_apps',json('[]'),
                '$.config.permissions.tools',json('[]'),'$.config.permissions.read_only',json('true'),
                '$.config.permissions.writable_roots',json('[]')) WHERE id=?1", [&actor.id])?;
            Ok(())
        }).unwrap();
        let config = system.config();
        system.shutdown().await;
        let reopened = ActorSystem::open_paused_with_factory(
            directory.path().join("data"),
            directory.path().join("workspace"),
            config,
            Arc::new(NoModels),
        )
        .await
        .unwrap();
        let retained = reopened.actor(&actor.id).unwrap();
        assert_eq!(retained.id, actor.id);
        assert!(reopened.store().history(&actor.id, 10).unwrap() == vec![history]);
        assert_eq!(
            reopened.store().message(&queued.id).unwrap().status,
            MessageStatus::Queued
        );
        let active = reopened.store().claim_next(&actor.id).unwrap().unwrap();
        let context = reopened.execution_context(&retained, &active).unwrap();
        let nested = retained.config.workspace.join("nested").join("result.txt");
        assert!(context.sandbox_policy.is_path_writable(&nested));
        let tools =
            crate::builtin_apps::create_apps(&retained, reopened.clone(), &reopened.config())
                .unwrap()
                .model_tool_specs()
                .unwrap();
        assert!(tools.iter().any(|tool| tool.name == "files__write_file"));
        assert!(tools.iter().any(|tool| tool.name == "actors__create"));
        assert!(!tools.iter().any(|tool| tool.name.starts_with("study__")));
        let stored: serde_json::Value = reopened
            .store()
            .stage_transaction(|tx| {
                let record: String = tx.query_row(
                    "SELECT record FROM actors WHERE id=?1",
                    [&actor.id],
                    |row| row.get(0),
                )?;
                Ok(serde_json::from_str(&record).unwrap())
            })
            .unwrap();
        assert!(stored["config"]["permissions"].get("app_access").is_none());
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn core_constructs_only_actors_and_hosted_actors_receive_enabled_mcp_apps() {
        let (_directory, system) = fixture().await;
        let server = crate::mcp::tests::server().await;
        let entry = system
            .app_registry()
            .install(
                "Example".into(),
                String::new(),
                crate::mcp::Source::Http {
                    url: server.url.clone(),
                    bearer_token: None,
                    timeouts: crate::mcp::Timeouts::default(),
                },
                &system.owner_app_context().unwrap(),
            )
            .await
            .unwrap();
        let core = system.core_device_actor().unwrap();
        let tools = crate::builtin_apps::create_apps(&core, system.clone(), &system.config())
            .unwrap()
            .model_tool_specs()
            .unwrap();
        assert!(tools.iter().all(|tool| tool.name.starts_with("actors__")));
        let actor = system.actor(FIXTURE_ACTOR_ID).unwrap();
        let tools = crate::builtin_apps::create_apps(&actor, system.clone(), &system.config())
            .unwrap()
            .model_tool_specs()
            .unwrap();
        assert!(tools.iter().any(|tool| tool.name.starts_with(&entry.id)));
        assert!(tools.iter().any(|tool| tool.name == "files__write_file"));
        system.shutdown().await;
    }

    #[tokio::test]
    async fn only_core_is_preset_and_creators_name_workers_with_uniform_apps() {
        use crate::app::App;
        let (directory, template) = fixture().await;
        let system = ActorSystem::open_paused_with_factory(
            directory.path().join("fresh-state"),
            directory.path().join("fresh-workspace"),
            template.config(),
            Arc::new(NoModels),
        )
        .await
        .unwrap();
        assert_eq!(
            system
                .list_visible()
                .unwrap()
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            ["core"]
        );
        let input = system
            .send_user("core", "Create workers".into(), vec![], None)
            .unwrap();
        system.store().claim_next("core").unwrap().unwrap();
        let context = system
            .execution_context(&system.actor("core").unwrap(), &input)
            .unwrap();
        let mut app = crate::actor_app::ActorApp::new("core".into(), system.clone());
        let mut created = Vec::new();
        for name in ["Parser repair", "Contract investigation"] {
            let result = app
                .execute_tool(
                    &crate::model_protocol::AgentToolCall {
                        id: name.into(),
                        name: "create".into(),
                        arguments: serde_json::json!({
                            "stage_id":null,"name":name,"workspace":null,
                            "instructions":null,"model":null,"initial_authorization":null
                        }),
                    },
                    &context,
                )
                .await
                .unwrap();
            let actor = system
                .actor(result.payload["id"].as_str().unwrap())
                .unwrap();
            assert_eq!(actor.config.name, name);
            assert_eq!(actor.parent_id.as_deref(), Some("core"));
            created.push(actor);
        }
        assert_eq!(created[0].config.apps, created[1].config.apps);
        #[cfg(not(target_os = "android"))]
        assert!(created[1].config.apps.iter().any(|app| app == "terminal"));
        let apps = crate::builtin_apps::create_apps(&created[1], system.clone(), &system.config())
            .unwrap();
        #[cfg(not(target_os = "android"))]
        assert!(
            apps.model_tool_specs()
                .unwrap()
                .iter()
                .any(|tool| tool.name == "terminal__terminal_exec")
        );
        assert!(
            app.execute_tool(
                &crate::model_protocol::AgentToolCall {
                    id: "select-apps".into(),
                    name: "create".into(),
                    arguments: serde_json::json!({
                        "stage_id":null,"name":"Extra","workspace":null,
                        "instructions":null,"model":null,"initial_authorization":null,"apps":["files"]
                    }),
                },
                &context
            )
            .await
            .is_err()
        );
        assert_eq!(system.store().actor_count().unwrap(), 3);
        system.shutdown().await;
        let reopened = ActorSystem::open_paused_with_factory(
            directory.path().join("fresh-state"),
            directory.path().join("fresh-workspace"),
            template.config(),
            Arc::new(NoModels),
        )
        .await
        .unwrap();
        assert_eq!(reopened.store().actor_count().unwrap(), 3);
        for actor in created {
            let retained = reopened.actor(&actor.id).unwrap();
            assert_eq!(retained.config.name, actor.config.name);
            assert_eq!(retained.config.apps, actor.config.apps);
        }
    }

    #[tokio::test]
    async fn actor_creation_rejects_used_names_and_lists_all_names_in_pages() {
        use crate::{app::App, model_protocol::AgentToolCall};
        let (directory, system) = fixture().await;
        let input = system
            .send_user("core", "Create workers".into(), vec![], None)
            .unwrap();
        system.store().claim_next("core").unwrap().unwrap();
        let context = system
            .execution_context(&system.actor("core").unwrap(), &input)
            .unwrap();
        let mut app = crate::actor_app::ActorApp::new("core".into(), system.clone());
        let create = |name: &str| AgentToolCall {
            id: Uuid::new_v4().to_string(),
            name: "create".into(),
            arguments: serde_json::json!({"stage_id":null,"name":name,"workspace":null,
                "instructions":null,"model":null,"initial_authorization":null}),
        };
        let result = app
            .execute_tool(&create("  Athena  "), &context)
            .await
            .unwrap();
        let athena = system
            .actor(result.payload["id"].as_str().unwrap())
            .unwrap();
        assert_eq!(athena.config.name, "Athena");
        system.store().set_archived(&athena.id, true).unwrap();
        let before = system.store().actor_count().unwrap();
        for name in [
            "Athena",
            "athena",
            " ATHENA ",
            &system.actor("core").unwrap().config.name,
        ] {
            let error = app.execute_tool(&create(name), &context).await.unwrap_err();
            assert!(matches!(
                error.downcast_ref::<ActorError>(),
                Some(ActorError::Conflict(_))
            ));
        }
        for name in ["", " \t\n"] {
            let error = app.execute_tool(&create(name), &context).await.unwrap_err();
            assert!(matches!(
                error.downcast_ref::<ActorError>(),
                Some(ActorError::Invalid(_))
            ));
        }
        let mut config = athena.config.clone();
        config.workspace = directory.path().join("must-not-be-created");
        let error = system
            .create_owned(FIXTURE_ACTOR_ID, config.clone())
            .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ActorError>(),
            Some(ActorError::Conflict(_))
        ));
        assert!(!config.workspace.exists());
        assert_eq!(system.store().actor_count().unwrap(), before);

        // All states and owners are discoverable through the existing bounded tool.
        let mut cursor = None;
        let mut names = Vec::new();
        loop {
            let result = app
                .execute_tool(
                    &AgentToolCall {
                        id: "names".into(),
                        name: "list".into(),
                        arguments: serde_json::json!({"stage_id":null,"cursor":cursor,"limit":1}),
                    },
                    &context,
                )
                .await
                .unwrap();
            for actor in result.payload["actors"].as_array().unwrap() {
                names.push(actor["name"].as_str().unwrap().to_owned());
            }
            cursor = result.payload["next_cursor"].as_u64();
            if cursor.is_none() {
                break;
            }
        }
        let mut expected = vec![
            "Athena".to_owned(),
            system.actor("core").unwrap().config.name,
            system.actor(FIXTURE_ACTOR_ID).unwrap().config.name,
        ];
        names.sort();
        expected.sort();
        assert_eq!(names, expected);
        system.shutdown().await;
    }

    #[tokio::test]
    async fn actor_name_check_is_atomic_and_scoped_to_the_account() {
        let (_directory, system) = fixture().await;
        let parent = system.actor(FIXTURE_ACTOR_ID).unwrap();
        let mut first = parent.clone();
        first.id = Uuid::new_v4().to_string();
        first.parent_id = Some(parent.id.clone());
        first.config.name = "Hermes".into();
        let mut second = first.clone();
        second.id = Uuid::new_v4().to_string();
        second.config.name = " HERMES ".into();
        let barrier = std::sync::Barrier::new(2);
        let outcomes = std::thread::scope(|scope| {
            let handles: Vec<_> = [first, second]
                .into_iter()
                .map(|actor| {
                    let barrier = &barrier;
                    let store = system.store();
                    let parent_id = &parent.id;
                    scope.spawn(move || {
                        store
                            .validate_new_actor_name(parent_id, &actor.config.name)
                            .unwrap();
                        barrier.wait();
                        store.save_created_actor(&actor, None, &[], None)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert!(matches!(
            outcomes
                .into_iter()
                .find_map(Result::err)
                .unwrap()
                .downcast_ref::<ActorError>(),
            Some(ActorError::Conflict(_))
        ));
        assert_eq!(system.store().actor_count().unwrap(), 3);

        // A foreign account's retained name cannot reserve names in this account.
        let hermes = system
            .list_for_actor(FIXTURE_ACTOR_ID)
            .unwrap()
            .into_iter()
            .find(|actor| actor.config.name.trim().eq_ignore_ascii_case("Hermes"))
            .unwrap();
        system.store().stage_transaction(|tx| {
            tx.execute_batch("CREATE TABLE IF NOT EXISTS stage_account_actors(actor TEXT PRIMARY KEY REFERENCES actors(id) ON DELETE CASCADE,account TEXT NOT NULL);")?;
            tx.execute("INSERT INTO stage_account_actors VALUES(?1,'local-account')", [&parent.id])?;
            tx.execute("INSERT INTO stage_account_actors VALUES(?1,'foreign-account')", [&hermes.id])?;
            Ok(())
        }).unwrap();
        let mut config = parent.config.clone();
        config.name = "Hermes".into();
        let local = system.create_owned(&parent.id, config.clone()).unwrap();
        // Descendant creators share the same account namespace.
        assert!(matches!(
            system
                .create_owned(&local.id, config)
                .unwrap_err()
                .downcast_ref::<ActorError>(),
            Some(ActorError::Conflict(_))
        ));
        system.shutdown().await;
    }

    #[tokio::test]
    async fn failed_workspace_preparation_does_not_change_actor_configuration() {
        let (directory, system) = fixture().await;
        let old = system.actor(FIXTURE_ACTOR_ID).unwrap().config;
        let blocked = directory.path().join("existing-file");
        std::fs::write(&blocked, "not a directory").unwrap();
        let mut proposed = old.clone();
        proposed.workspace = blocked.join("child");
        assert!(
            system
                .update_owned(FIXTURE_ACTOR_ID, proposed)
                .await
                .is_err()
        );
        assert_eq!(system.actor(FIXTURE_ACTOR_ID).unwrap().config, old);
        assert!(!system.inner.reconfiguring.load(Ordering::Acquire));
    }
    #[tokio::test]
    async fn shutdown_releases_only_its_owned_temporary_instance() {
        let (directory, system) = fixture().await;
        let instance = system
            .inner
            .temporary
            .lock()
            .as_ref()
            .unwrap()
            .path()
            .to_path_buf();
        let survivor =
            crate::runtime_temp::RuntimeTemp::create(instance.parent().unwrap().parent().unwrap())
                .unwrap();
        let survivor_path = survivor.path().to_path_buf();
        let context = system.owner_app_context().unwrap();
        assert!(context.sandbox_policy.builtin_resources.is_empty());
        assert!(instance.is_dir());
        system.shutdown().await;
        assert!(!instance.exists());
        assert!(survivor_path.is_dir());
        assert!(system.infrastructure_dir().is_dir());
        assert!(directory.path().exists());
        system.shutdown().await;
        assert!(survivor_path.is_dir());
    }
}
