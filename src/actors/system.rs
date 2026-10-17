//! Actor identities, capability checks and asynchronous mailbox routing.
use super::{
    permissions::{canonical_path, check_child, normalize_config, protect_private_path},
    store::ActorStore,
    *,
};
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
pub struct ActorControl {
    pub wake: Notify,
    pub stop: AtomicBool,
    pub cancelled: Mutex<HashSet<String>>,
}
impl Default for ActorControl {
    fn default() -> Self {
        Self {
            wake: Notify::new(),
            stop: AtomicBool::new(false),
            cancelled: Mutex::new(HashSet::new()),
        }
    }
}
struct Worker {
    control: Arc<ActorControl>,
    task: JoinHandle<()>,
}
struct Inner {
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
    pub(crate) fn shared_core(&self) -> Option<Arc<crate::stage::shared_runtime::Controller>> {
        self.inner
            .core
            .read()
            .as_ref()
            .and_then(std::sync::Weak::upgrade)
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
        let mut actor = self.actor(crate::stage::shared::ACTOR_ID)?;
        if actor.config.permissions.delegation_apps.is_none() {
            actor.config.permissions.delegation_apps = Some(actor.config.apps.clone());
            actor.config.apps = vec!["actors".into()];
            self.store().save_actor(&actor)?;
        }
        self.start_worker(crate::stage::shared::ACTOR_ID)
    }
    /// The account Actor's identity is shared; filesystem/App installation
    /// policy belongs to the device exposing the instance.
    pub(crate) fn core_device_actor(&self) -> Result<ActorRecord> {
        let mut actor = self.actor(crate::stage::shared::ACTOR_ID)?;
        let identity = actor.config.permissions.clone();
        actor.config.workspace = self.device_workspace().to_owned();
        actor.config.apps = vec!["actors".into()];
        actor.config.permissions = self.device_permissions()?;
        actor.config.permissions.app_access = identity.app_access.clone();
        actor.config.permissions.delegation_apps = Some(
            identity
                .delegation_apps
                .as_deref()
                .unwrap_or(&actor.config.apps)
                .iter()
                .filter(|app| crate::builtin_apps::DEVICE_APPS.contains(&app.as_str()))
                .cloned()
                .collect(),
        );
        actor.config.permissions.read_only |= identity.read_only;
        if actor.config.permissions.read_only {
            actor.config.permissions.writable_roots.clear();
        }
        if super::permissions::approval_strength(identity.approval_mode)
            > super::permissions::approval_strength(actor.config.permissions.approval_mode)
        {
            actor.config.permissions.approval_mode = identity.approval_mode;
        }
        actor.config.permissions.allow_children &= identity.allow_children;
        if let Some(tools) = identity.tools {
            actor.config.permissions.tools = Some(
                tools
                    .into_iter()
                    .filter(|tool| {
                        actor
                            .config
                            .permissions
                            .tools
                            .as_ref()
                            .is_none_or(|device| device.contains(tool))
                    })
                    .collect(),
            );
        }
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
        let mut actor = self.store().actor(&message.actor_id)?;
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
                    .actor_grant(
                        authority.peer,
                        &authority.grant_actor,
                        chrono::Utc::now().timestamp_millis(),
                    )
                    .map_err(|_| forbidden("Remote Actor capability is unavailable"))?,
                )
            };
            if authority.grant_actor != actor.id
                && granted.as_ref().is_none_or(|grant| !grant.delegate)
            {
                return Err(forbidden("Remote authority cannot delegate"));
            }
            let ceiling = message
                .delegation_grant
                .as_ref()
                .ok_or_else(|| forbidden("Remote request has no immutable capability ceiling"))?;
            let bound_apps = ceiling
                .permissions
                .app_access
                .as_deref()
                .unwrap_or(&ceiling.apps);
            let allowed = super::permissions::direct_apps(&actor.config)
                .iter()
                .filter(|app| ceiling.apps.contains(app) && bound_apps.contains(app))
                .cloned()
                .collect();
            actor.config.permissions.app_access = Some(allowed);
            let permission = &mut actor.config.permissions;
            permission.read_only |= ceiling.permissions.read_only
                || granted.as_ref().is_some_and(|grant| grant.read_only);
            if super::permissions::approval_strength(permission.approval_mode)
                < super::permissions::approval_strength(ceiling.permissions.approval_mode)
            {
                permission.approval_mode = ceiling.permissions.approval_mode;
            }
            let intersect = |current: &[PathBuf], bound: &[PathBuf]| -> Vec<PathBuf> {
                let mut roots = Vec::new();
                for a in current {
                    for b in bound {
                        if a.starts_with(b) {
                            roots.push(a.clone());
                        } else if b.starts_with(a) {
                            roots.push(b.clone());
                        }
                    }
                }
                roots.sort();
                roots.dedup();
                roots
            };
            permission.readable_roots = intersect(
                &permission.readable_roots,
                &ceiling.permissions.readable_roots,
            );
            permission.writable_roots = if permission.read_only {
                vec![]
            } else {
                intersect(
                    &permission.writable_roots,
                    &ceiling.permissions.writable_roots,
                )
            };
            permission.allow_children &=
                ceiling.permissions.allow_children && authority.reply_for.is_some();
            if let Some(apps) = &mut permission.delegation_apps {
                let bound = ceiling
                    .permissions
                    .delegation_apps
                    .as_ref()
                    .unwrap_or(&ceiling.apps);
                apps.retain(|app| bound.contains(app));
            }
            permission
                .message_targets
                .retain(|target| ceiling.permissions.message_targets.contains(target));
            permission.tools = match (&permission.tools, &ceiling.permissions.tools) {
                (Some(a), Some(b)) => {
                    Some(a.iter().filter(|tool| b.contains(tool)).cloned().collect())
                }
                (None, Some(b)) => Some(b.clone()),
                (Some(a), None) => Some(a.clone()),
                (None, None) => None,
            };
            if granted.is_some_and(|grant| !grant.delegate)
                && let Some(apps) = &mut permission.app_access
            {
                apps.retain(|app| app != "actors");
            }
            actor.config = normalize_config(actor.config)?;
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
        store.recover()?;
        let system = Self {
            inner: Arc::new(Inner {
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
        // Retain existing identities/history while moving installation to the
        // device. Former installation restrictions remain explicit action grants.
        for mut actor in system.store().actors()? {
            let previous = actor.config.clone();
            super::migrations::retire_study_grants(
                &mut actor.config.apps,
                &mut actor.config.permissions.app_access,
                &mut actor.config.permissions.delegation_apps,
                &mut actor.config.permissions.tools,
            );
            if actor.id != crate::stage::shared::ACTOR_ID {
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
        actor.config.permissions = self.device_permissions().unwrap();
        self.prepare_dirs(id).unwrap();
        self.store().save_actor(&actor).unwrap();
    }
    fn device_permissions(&self) -> Result<ActorPermissions> {
        let mut permissions = ActorPermissions::workspace(self.inner.workspace.clone(), false);
        // This is a device ceiling; the Owner's persisted Actor mode supplies review.
        permissions.approval_mode = ApprovalMode::None;
        if let Some(home) = std::env::home_dir() {
            let skills = home.join(".agents/skills");
            if skills.is_dir() {
                permissions.readable_roots.push(canonical_path(&skills)?);
            }
        }
        permissions.readable_roots.sort();
        permissions.readable_roots.dedup();
        Ok(permissions)
    }
    fn hosted_config(&self, mut config: ActorConfig) -> Result<ActorConfig> {
        let apps: Vec<String> = crate::builtin_apps::DEVICE_APPS
            .iter()
            .map(|app| (*app).to_owned())
            .collect();
        if config.apps != apps {
            // Preserve the old grant boundary, not a separate App installation.
            if config.permissions.app_access.is_none() {
                config.permissions.app_access = Some(config.apps.clone());
            } else if let Some(access) = &mut config.permissions.app_access {
                access.retain(|app| config.apps.contains(app));
            }
            if config.permissions.delegation_apps.is_none() {
                config.permissions.delegation_apps = Some(config.apps.clone());
            }
            config.apps = apps;
        }
        normalize_config(config)
    }
    pub fn infrastructure_dir(&self) -> PathBuf {
        self.inner.data_dir.clone()
    }
    pub fn send_stage_user(
        &self,
        target: crate::stage::ActorAddress,
        content: String,
        key: &str,
    ) -> Result<crate::stage::protocol::Envelope> {
        self.send_stage_user_with_attachments(target, content, Vec::new(), key)
    }
    pub fn send_stage_user_with_attachments(
        &self,
        target: crate::stage::ActorAddress,
        content: String,
        attachments: Vec<crate::resources::Resource>,
        key: &str,
    ) -> Result<crate::stage::protocol::Envelope> {
        if self.inner.stopping.load(Ordering::Acquire)
            || self.inner.reconfiguring.load(Ordering::Acquire)
        {
            return Err(forbidden("Actor service is not accepting input"));
        }
        let local = self
            .store()
            .stage_local_id()
            .map_err(|error| miette::miette!("{error}"))?;
        crate::stage::delivery::Deliveries::new(self.store(), local)
            .queue(
                &self.core_device_actor()?,
                None,
                target,
                (content, attachments),
                now(),
                key,
            )
            .map_err(|error| miette::miette!("{error}"))
    }
    pub fn send_stage_from_actor(
        &self,
        sender: &str,
        current_id: &str,
        target: crate::stage::ActorAddress,
        content: String,
        call_id: &str,
    ) -> Result<crate::stage::protocol::Envelope> {
        let current = self.store().message(current_id)?;
        if current.actor_id != sender
            || current.status != MessageStatus::Running
            || current.cancel_requested
        {
            return Err(forbidden("Actor message authority is not valid"));
        }
        let source = self.execution_actor(&current)?;
        let owner = source.id == crate::stage::shared::ACTOR_ID
            && matches!(current.reply_to, ReplyTarget::User)
            && current.remote_authority.is_none()
            && !source.config.permissions.read_only;
        if !owner
            && !source
                .config
                .permissions
                .message_targets
                .contains(&target.conversation_url())
        {
            return Err(forbidden("Remote recipient is outside Actor grants"));
        }
        let local = self
            .store()
            .stage_local_id()
            .map_err(|error| miette::miette!("{error}"))?;
        crate::stage::delivery::Deliveries::new(self.store(), local)
            .queue(
                &source,
                Some(&current),
                target,
                (content, Vec::new()),
                now(),
                call_id,
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
        crate::stage::reset::replace(self.store(), record, &root)
            .map_err(|error| miette::miette!("{error}"))?;
        crate::stage::reset::cleanup(self).map_err(|error| miette::miette!("{error}"))?;
        drop(pause);
        Ok(())
    }
    fn initial_core(&self) -> Result<ActorRecord> {
        let workspace = self.inner.workspace.clone();
        let mut permissions = self.device_permissions()?;
        permissions.approval_mode = ApprovalMode::Auto;
        permissions.delegation_apps = Some(
            crate::builtin_apps::DEVICE_APPS
                .iter()
                .map(|app| (*app).to_owned())
                .collect(),
        );
        let root = ActorRecord {
            id: crate::stage::shared::ACTOR_ID.into(),
            parent_id: None,
            depth: 0,
            config: normalize_config(ActorConfig {
                name: "Daat Locus".into(),
                workspace,
                apps: vec!["actors".into()],
                model: None,
                instructions: String::new(),
                permissions,
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
            || (id == crate::stage::shared::ACTOR_ID
                && self.shared_core().is_none_or(|core| !core.permitted()))
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
        if passivated {
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

    /// Facts needed to review a delegation, without provider secrets, another
    /// Actor's history, or instructions. A result is judged against the request
    /// that caused it, rather than treating another Actor's answer as user intent.
    pub fn approval_facts(
        &self,
        actor: &ActorRecord,
        message: &MessageRecord,
        call: &crate::model_protocol::AgentToolCall,
    ) -> Result<serde_json::Value> {
        let target = if call.name == "actors__send" {
            let id = call
                .arguments
                .get("actor_id")
                .and_then(|id| id.as_str())
                .ok_or_else(|| invalid("Missing recipient Actor"))?;
            let recipient = self.actor(id)?;
            if !self.can_message(actor, &recipient) {
                return Err(forbidden(
                    "Recipient is outside the Actor's messaging grants",
                ));
            }
            Some(serde_json::json!({
                "id": recipient.id,
                "name": recipient.config.name,
                "workspace": recipient.config.workspace,
                "apps": recipient.config.apps,
                "grants": recipient.config.permissions,
            }))
        } else {
            None
        };
        let originating_request =
            if matches!(message.kind, MessageKind::Result | MessageKind::Progress) {
                message
                    .in_reply_to
                    .as_deref()
                    .and_then(|id| self.store().message(id).ok())
                    .filter(|original| original.actor_id == actor.id)
                    .map(|original| {
                        serde_json::json!({
                            "id": original.id, "content": original.content,
                            "source": original.source,
                        })
                    })
            } else {
                None
            };
        Ok(serde_json::json!({ "recipient": target, "originating_request": originating_request }))
    }

    pub fn requested_tool_context(
        &self,
        base: &AppToolExecutionContext,
        requested: &ToolPermissionRequest,
    ) -> Result<AppToolExecutionContext> {
        let call = base
            .actor_call
            .as_ref()
            .ok_or_else(|| forbidden("Missing tool caller authority"))?;
        let actor = self.record_for_tool(&call.actor_id, call)?;
        if actor.config.permissions.read_only && !requested.writable_roots.is_empty() {
            return Err(forbidden("A read-only identity cannot obtain write access"));
        }
        let mut context = base.clone();
        let remote = self
            .store()
            .message(&call.message_id)?
            .remote_authority
            .is_some();
        for path in &requested.readable_roots {
            let path = canonical_path(path)?;
            if context
                .sandbox_policy
                .filesystem
                .deny_read_paths
                .iter()
                .any(|denied| path.starts_with(denied))
            {
                return Err(forbidden(
                    "Private service or Actor data cannot be granted to this operation",
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
            let path = canonical_path(path)?;
            if context
                .sandbox_policy
                .filesystem
                .deny_read_paths
                .iter()
                .chain(context.sandbox_policy.filesystem.deny_write_paths.iter())
                .any(|denied| path.starts_with(denied))
            {
                return Err(forbidden(
                    "Private service or Actor data cannot be granted to this operation",
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
        if config.workspace.starts_with(&self.inner.data_dir)
            || config.workspace.starts_with(&self.inner.delivery_dir)
        {
            return Err(invalid(
                "Private service storage cannot be an Actor workspace",
            ));
        }
        if let Some(model) = &config.model
            && !self.inner.config.read().models.contains_key(model)
        {
            return Err(invalid("Unknown model configuration"));
        }
        for id in &config.permissions.message_targets {
            self.actor(id)?;
        }
        Ok(())
    }
    fn create(
        &self,
        parent: &ActorRecord,
        config: ActorConfig,
        workspace_policy: Option<&crate::sandbox::RuntimeSandboxPolicy>,
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
        let config = self.hosted_config(config)?;
        self.validate_config(&config)?;
        crate::file_io::ensure_directory(
            workspace_policy.unwrap_or(&crate::sandbox::RuntimeSandboxPolicy::disabled()),
            &config.workspace,
        )?;
        let actor = ActorRecord {
            context: Default::default(),
            id: Uuid::new_v4().to_string(),
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
        self.store().save_actor(&actor)?;
        crate::stage::ledger::inherit_account_ownership(self.store(), &parent.id, &actor.id)
            .map_err(|error| miette::miette!("{error}"))?;
        Ok(actor)
    }
    pub fn create_owned(&self, parent: &str, config: ActorConfig) -> Result<ActorRecord> {
        let parent = self.actor(parent)?;
        self.create(&parent, config, None)
    }
    pub fn create_from_actor(
        &self,
        sender: &str,
        config: ActorConfig,
        context: &AppToolExecutionContext,
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
        self.record_for_tool(sender, call)?;
        let mut ceiling = self.record_for_tool(sender, call)?.config;
        ceiling.permissions.readable_roots =
            context.sandbox_policy.filesystem.readable_roots.clone();
        ceiling.permissions.writable_roots = context
            .sandbox_policy
            .filesystem
            .writable_roots
            .iter()
            .map(|root| root.root.clone())
            .collect();
        if !context.sandbox_policy.is_path_readable(&config.workspace)
            || (!config.permissions.read_only
                && !context.sandbox_policy.is_path_writable(&config.workspace))
        {
            return Err(forbidden(
                "Actor workspace exceeds this operation's authorized scope",
            ));
        }
        if parent.config.permissions.read_only && !config.workspace.exists() {
            return Err(forbidden(
                "Read-only Actors cannot create a workspace directory",
            ));
        }
        check_child(&ceiling, &config)?;
        self.create(&parent, config, Some(&context.sandbox_policy))
    }
    pub fn list_visible(&self) -> Result<Vec<ActorSummary>> {
        Ok(self
            .store()
            .actors()?
            .into_iter()
            .map(|a| a.summary())
            .collect())
    }
    pub fn list_for_actor(&self, sender: &str) -> Result<Vec<ActorRecord>> {
        let actor = self.actor(sender)?;

        Ok(self
            .store()
            .actors()?
            .into_iter()
            .filter(|target| self.can_message(&actor, target))
            .collect())
    }
    fn can_message(&self, sender: &ActorRecord, target: &ActorRecord) -> bool {
        sender.id == target.id
            || (sender.id == crate::stage::shared::ACTOR_ID
                && !sender.config.permissions.read_only
                && self
                    .store()
                    .current_message(&sender.id)
                    .ok()
                    .flatten()
                    .is_some_and(|message| matches!(message.reply_to, ReplyTarget::User)))
            || sender.parent_id.as_deref() == Some(&target.id)
            || target.parent_id.as_deref() == Some(&sender.id)
            || sender
                .config
                .permissions
                .message_targets
                .contains(&target.id)
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
        let actor = self.execution_actor(&input)?;
        if let Some(policy) = &call.device_policy {
            if id != crate::stage::shared::ACTOR_ID {
                return Err(forbidden("Device App policy belongs to the shared Actor"));
            }
            if input.remote_authority.is_some() {
                super::permissions::check_recipient(&actor.config, policy)?;
            }
            let actor = self.core_device_actor()?;
            if super::permissions::check_recipient(&actor.config, policy).is_err() {
                return Err(forbidden("Device App grants changed during the operation"));
            }
            let mut actor = actor;
            actor.config = policy.clone();
            return Ok(actor);
        }
        if input.remote_authority.is_none()
            && input.kind == MessageKind::Request
            && matches!(input.source, MessageSource::Actor { .. })
        {
            let bound = input
                .delegation_grant
                .as_ref()
                .ok_or_else(|| forbidden("Delegated request has no immutable authority"))?;
            let mut ceiling = actor.config.clone();
            ceiling.apps = bound.apps.clone();
            ceiling.permissions = bound.permissions.clone();
            if !bound.permissions.allow_children && actor.config.permissions.allow_children {
                return Err(forbidden("Actor capability changed after delegation"));
            }
            super::permissions::check_recipient(&ceiling, &actor.config)?;
        }
        Ok(actor)
    }
    pub fn delegated_request(&self, sender: &str, id: &str) -> Result<MessageRecord> {
        let message = self.store().message(id)?;
        if !matches!(&message.source, MessageSource::Actor { actor_id } if actor_id == sender)
            || !matches!(&message.reply_to, ReplyTarget::Actor { actor_id, .. } if actor_id == sender)
            || message.kind != MessageKind::Request
        {
            return Err(forbidden("Request was not delegated by this Actor"));
        }
        let source = self.app_actor(sender)?;
        let target = self.actor(&message.actor_id)?;
        if !self.can_message(&source, &target) {
            return Err(forbidden("Recipient grant is no longer available"));
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
        if sender == crate::stage::shared::ACTOR_ID
            && self
                .shared_core()
                .is_none_or(|core| !core.execution_permitted())
        {
            return Err(forbidden("Core worker no longer owns execution"));
        }
        let delivery = self.store().deliver(sender, input, call, content)?;
        if delivery.status == MessageStatus::Queued {
            self.wake(&delivery.actor_id);
        }
        Ok(delivery)
    }
    pub fn send_from_actor(
        &self,
        sender: &str,
        current_id: &str,
        target: &str,
        content: String,
    ) -> Result<MessageRecord> {
        self.send_from_actor_with_context(sender, current_id, target, content, None)
    }
    pub(crate) fn send_from_actor_with_context(
        &self,
        sender: &str,
        current_id: &str,
        target: &str,
        content: String,
        context: Option<&AppToolExecutionContext>,
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
            source.config.permissions.readable_roots =
                context.sandbox_policy.filesystem.readable_roots.clone();
            source.config.permissions.writable_roots = context
                .sandbox_policy
                .filesystem
                .writable_roots
                .iter()
                .map(|root| root.root.clone())
                .collect();
        }
        let recipient = self.actor(target)?;
        if !self.can_message(&source, &recipient) {
            return Err(forbidden(
                "Recipient is outside the Actor's messaging grants",
            ));
        }
        if content.is_empty() || content.len() > 64 * 1024 {
            return Err(invalid("Invalid Actor message size"));
        }
        let owner_routing = source.id == crate::stage::shared::ACTOR_ID
            && matches!(current.reply_to, ReplyTarget::User)
            && !source.config.permissions.read_only;
        if !owner_routing && current.remote_authority.is_none() {
            super::permissions::check_recipient(&source.config, &recipient.config)?;
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
        message.delegation_grant = Some(DelegationGrant {
            apps: recipient.config.apps,
            permissions: recipient.config.permissions,
        });
        if current.remote_authority.is_some() {
            let ceiling = message
                .delegation_grant
                .as_mut()
                .ok_or_else(|| forbidden("Missing delegation ceiling"))?;
            ceiling
                .apps
                .retain(|app| super::permissions::delegation_apps(&source.config).contains(app));
            ceiling.permissions.read_only |= source.config.permissions.read_only;
            ceiling.permissions.allow_children = false;
            ceiling.permissions.readable_roots.retain(|root| {
                source
                    .config
                    .permissions
                    .readable_roots
                    .iter()
                    .any(|bound| root.starts_with(bound))
            });
            if !ceiling
                .permissions
                .readable_roots
                .iter()
                .any(|root| recipient.config.workspace.starts_with(root))
            {
                return Err(forbidden(
                    "Delegation exceeds remote filesystem capabilities",
                ));
            }
            ceiling.permissions.writable_roots.retain(|root| {
                !ceiling.permissions.read_only
                    && source
                        .config
                        .permissions
                        .writable_roots
                        .iter()
                        .any(|bound| root.starts_with(bound))
            });
            ceiling
                .permissions
                .message_targets
                .retain(|target| source.config.permissions.message_targets.contains(target));
            if let Some(source_tools) = &source.config.permissions.tools {
                ceiling.permissions.tools = Some(ceiling.permissions.tools.as_ref().map_or_else(
                    || source_tools.clone(),
                    |tools| {
                        tools
                            .iter()
                            .filter(|tool| source_tools.contains(tool))
                            .cloned()
                            .collect()
                    },
                ));
            }
            message.remote_authority = current.remote_authority.clone();
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
            delegation_grant: None,
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
    pub async fn steer_queued(
        &self,
        actor: &str,
        message: &str,
        target: &str,
        expected: i64,
    ) -> Result<MessageRecord> {
        if actor == crate::stage::shared::ACTOR_ID
            && let Some(core) = self.shared_core()
        {
            return core
                .steer(message, target, expected)
                .await
                .map_err(|error| miette::miette!("{error}"));
        }
        let result = self
            .store()
            .steer_queued(actor, message, target, expected)?;
        self.wake(actor);
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
    pub async fn update_owned(&self, id: &str, config: ActorConfig) -> Result<ActorRecord> {
        if id == crate::stage::shared::ACTOR_ID && self.shared_core().is_some() {
            return Err(forbidden(
                "Shared Actor only accepts model and approval preferences",
            ));
        }
        self.update_owned_with(id, |_| normalize_config(config))
            .await
    }
    /// Apply composer preferences to the latest grants under the lifecycle lock.
    pub async fn patch_owned_preferences(
        &self,
        id: &str,
        model: Option<Option<String>>,
        approval_mode: Option<ApprovalMode>,
    ) -> Result<ActorRecord> {
        if id == crate::stage::shared::ACTOR_ID
            && let Some(core) = self.shared_core()
        {
            core.preferences(model, approval_mode)
                .await
                .map_err(|error| miette::miette!("{error}"))?;
            return self.actor(id);
        }
        self.apply_owned_preferences(id, model, approval_mode).await
    }
    pub(crate) async fn apply_owned_preferences(
        &self,
        id: &str,
        model: Option<Option<String>>,
        approval_mode: Option<ApprovalMode>,
    ) -> Result<ActorRecord> {
        self.update_owned_with(id, |mut config| {
            if let Some(model) = model {
                config.model = model;
            }
            if let Some(mode) = approval_mode {
                config.permissions.approval_mode = mode;
            }
            let checked = normalize_config(config.clone())?;
            if checked.workspace != config.workspace
                || checked.permissions.readable_roots != config.permissions.readable_roots
                || checked.permissions.writable_roots != config.permissions.writable_roots
            {
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
        let worker = self.inner.workers.lock().remove(id);
        if let Some(worker) = worker {
            self.stop_actor_worker(id, worker).await?;
        }
        actor.config = config;
        actor.status = ActorStatus::Idle;
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
                actor_id: actor.id.clone(),
                message_id: message.id.clone(),
                private_roots: vec![],
                device_policy: device_policy.clone(),
            },
        )?;
        let config = self.config();
        let paths = daat_locus_paths_sync();
        let mut denied = vec![
            self.inner.data_dir.clone(),
            paths.config_dir(),
            paths.runtime_dir(),
            paths.state_dir(),
            paths.codex_auth_dir(),
            paths.opencode_auth_dir(),
            paths.artifacts_dir(),
            paths.logs_dir(),
        ];
        let mut private_roots = vec![
            self.inner.data_dir.clone(),
            self.inner.delivery_dir.clone(),
            paths.root().to_path_buf(),
        ];
        if let Some(root) = std::env::var_os("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT") {
            let root = canonical_path(std::path::Path::new(&root))?;
            denied.push(root.clone());
            private_roots.push(root);
        }
        // Delivery files are separate from state/credentials and are actor-local.
        denied.retain(|path| !self.actor_data_dir(&actor.id).starts_with(path));
        for id in self.store().actor_ids()? {
            if id != actor.id {
                denied.push(self.actor_data_dir(&id));
            }
        }
        for provider in config.providers.values() {
            match provider {
                ProviderConfig::OpenaiCodexOauth { auth_file, .. }
                | ProviderConfig::OpenCodeConsoleOauth { auth_file, .. } => {
                    let value = resolve_env_reference(auth_file);
                    if !value.trim().is_empty() {
                        let cwd = std::env::current_dir().into_diagnostic()?;
                        let path =
                            canonical_path(&crate::sandbox::RuntimeSandboxPolicy::resolve_path(
                                std::path::Path::new(&value),
                                Some(&cwd),
                            ))?;
                        private_roots.push(path.clone());
                        denied.push(path);
                    }
                }
                _ => {}
            }
        }
        let mut protected_env = config.protected_secret_env_vars();
        #[cfg(target_os = "android")]
        if let Some(application_dir) = daat_locus_paths_sync().root().parent() {
            // Native WebView cookies and platform stores share the app UID. Full
            // access to an App operation never grants these framework credentials.
            for entry in std::fs::read_dir(application_dir).into_diagnostic()? {
                let entry = entry.into_diagnostic()?;
                // Service storage already uses the Actor-bound delivery policy,
                // which permits only this Actor's ledger-backed resource bytes.
                if entry.file_name() != "workspace" && entry.path() != paths.root() {
                    denied.push(entry.path());
                }
            }
        }
        let inherited_safe = [
            "PATH",
            "PATHEXT",
            "SYSTEMROOT",
            "WINDIR",
            "COMSPEC",
            "TEMP",
            "TMP",
            "HOME",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
            "LANG",
            "LC_ALL",
            "LC_CTYPE",
            "TERM",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
        ];
        protected_env.extend(std::env::vars_os().filter_map(|(key, _)| {
            let key = key.to_string_lossy();
            let upper = key.to_ascii_uppercase();
            if !inherited_safe.contains(&upper.as_str()) {
                Some(key.into_owned())
            } else {
                None
            }
        }));
        let tool_output_max_tokens = config
            .models
            .get(config.actor_model_name(actor.config.model.as_deref()))
            .map_or(4096, |model| model.tool_output_max_tokens);
        let mut sandbox_policy = super::permissions::policy(
            &actor.config,
            &self.actor_data_dir(&actor.id),
            denied,
            protected_env,
        );
        if let Some(base) = std::env::var_os("DAAT_LOCUS_ACCOUNT_STORAGE_ROOT") {
            // Protect the entire namespace, including profiles created after
            // this context. Only this Actor's immutable deliveries are readable.
            // Enumerating sibling profiles would leave a creation-time race.
            if let Some(scope) = sandbox_policy.filesystem.private_delivery.as_mut() {
                scope.root = canonical_path(std::path::Path::new(&base))?;
            }
        }
        Ok(AppToolExecutionContext {
            execution_cwd: actor.config.workspace.clone(),
            sandbox_policy,
            tool_output_max_tokens,
            turn_epoch: message.seq as u64,
            actor_call: Some(ActorCallContext {
                actor_id: actor.id,
                message_id: message.id.clone(),
                private_roots,
                device_policy: device_policy.clone(),
            }),
        })
    }
    pub fn message_content(
        &self,
        actor: &ActorRecord,
        message: &MessageRecord,
        _context: &AppToolExecutionContext,
    ) -> Result<AgentContent> {
        let mut text = format!(
            "Message id: {}\nSource: {}\nKind: {:?}\nRelated message: {}\n\n{}",
            message.id,
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
        if matches!(message.kind, MessageKind::Result | MessageKind::Progress)
            && let Some(id) = message.in_reply_to.as_deref()
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
                "\nAttached resource {}: {}. Read its contents with actors__read_resource.",
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
                | ProviderConfig::OpenCodeConsoleOauth { auth_file, .. } => {
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

    struct NoModels;
    impl ModelFactory for NoModels {
        fn build(&self, _: &str, _: &Config) -> Result<Arc<dyn ModelProvider>> {
            Err(invalid("A paused test must never invoke a provider"))
        }
    }
    async fn fixture() -> (tempfile::TempDir, ActorSystem) {
        let directory = tempfile::tempdir().unwrap();
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
        for _ in 0..10 {
            let child = system
                .create_owned(&parent.id, parent.config.clone())
                .unwrap();
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
    async fn retired_study_grants_are_removed_on_reopen_without_expanding_permissions() {
        let (directory, system) = fixture().await;
        let mut actor = system.actor(FIXTURE_ACTOR_ID).unwrap();
        actor.config.apps = vec!["files".into(), "study".into()];
        actor.config.permissions.app_access = Some(vec!["files".into(), "study".into()]);
        actor.config.permissions.delegation_apps = Some(vec!["study".into()]);
        actor.config.permissions.tools =
            Some(vec!["files__read_file".into(), "study__create_node".into()]);
        system.store().save_actor(&actor).unwrap();
        let queued = system
            .send_user(&actor.id, "Retain this request".into(), vec![], None)
            .unwrap();
        let history = crate::model_protocol::AgentMessage::user("Retain the original history");
        system
            .store()
            .append_history(&actor.id, &queued.id, std::slice::from_ref(&history))
            .unwrap();
        let mut core = system.actor("core").unwrap();
        core.config.permissions.delegation_apps = Some(vec!["study".into()]);
        system.store().save_actor(&core).unwrap();
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
        assert_eq!(
            retained.config.permissions.app_access,
            Some(vec!["files".into()])
        );
        assert_eq!(retained.config.permissions.delegation_apps, Some(vec![]));
        assert_eq!(
            retained.config.permissions.tools,
            Some(vec!["files__read_file".into()])
        );
        assert_eq!(
            reopened
                .actor("core")
                .unwrap()
                .config
                .permissions
                .delegation_apps,
            Some(vec![])
        );
        assert!(reopened.store().history(&actor.id, 10).unwrap() == vec![history]);
        let request = reopened.store().message(&queued.id).unwrap();
        assert_eq!(request.content, "Retain this request");
        assert_eq!(request.status, MessageStatus::Queued);
        let apps =
            crate::builtin_apps::create_apps(&retained, reopened.clone(), &reopened.config())
                .unwrap();
        let tools = apps.model_tool_specs().unwrap();
        assert!(tools.iter().any(|tool| tool.name == "files__read_file"));
        assert!(!tools.iter().any(|tool| tool.name.starts_with("study__")));
        assert!(!tools.iter().any(|tool| tool.name == "files__write_file"));
        reopened.shutdown().await;
    }

    #[tokio::test]
    async fn legacy_actor_installation_becomes_an_action_grant_without_losing_history() {
        let (directory, system) = fixture().await;
        let mut actor = system.actor(FIXTURE_ACTOR_ID).unwrap();
        actor.config.name = "Existing investigation".into();
        actor.config.apps = vec!["files".into()];
        actor.config.permissions.app_access = None;
        actor.config.permissions.delegation_apps = None;
        system.store().save_actor(&actor).unwrap();
        let queued = system
            .send_user(&actor.id, "Preserve this input".into(), vec![], None)
            .unwrap();
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
        assert_eq!(retained.config.name, "Existing investigation");
        assert_eq!(
            reopened.store().message(&queued.id).unwrap().content,
            "Preserve this input"
        );
        assert_eq!(
            retained.config.permissions.app_access,
            Some(vec!["files".into()])
        );
        let apps =
            crate::builtin_apps::create_apps(&retained, reopened.clone(), &reopened.config())
                .unwrap();
        let tools = apps.model_tool_specs().unwrap();
        assert!(tools.iter().any(|tool| tool.name == "files__write_file"));
        assert!(!tools.iter().any(|tool| tool.name.starts_with("terminal__")));
        assert!(!tools.iter().any(|tool| tool.name == "actors__create"));
        assert!(tools.iter().any(|tool| tool.name == "actors__read_history"));
        let mut candidate = retained.config.clone();
        candidate.permissions.app_access = None;
        assert!(check_child(&retained.config, &candidate).is_err());
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
        for (name, read_only) in [("Parser repair", false), ("Contract investigation", true)] {
            let result = app
                .execute_tool(
                    &crate::model_protocol::AgentToolCall {
                        id: name.into(),
                        name: "create".into(),
                        arguments: serde_json::json!({
                            "stage_id":null,"name":name,"workspace":null,"read_only":read_only,
                            "instructions":null,"model":null
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
            assert_eq!(actor.config.permissions.read_only, read_only);
            created.push(actor);
        }
        assert_eq!(created[0].config.apps, created[1].config.apps);
        #[cfg(not(target_os = "android"))]
        assert!(created[1].config.apps.iter().any(|app| app == "terminal"));
        let mut apps =
            crate::builtin_apps::create_apps(&created[1], system.clone(), &system.config())
                .unwrap();
        assert!(
            !apps
                .model_tool_specs()
                .unwrap()
                .iter()
                .any(|tool| tool.name.starts_with("terminal__"))
        );
        let readonly_input = system
            .send_user(&created[1].id, "Inspect".into(), vec![], None)
            .unwrap();
        system.store().claim_next(&created[1].id).unwrap().unwrap();
        let readonly_context = system
            .execution_context(&created[1], &readonly_input)
            .unwrap();
        assert!(apps.execute_tool(&crate::model_protocol::AgentToolCall {
            id: "forbidden-process".into(), name: "terminal__terminal_exec".into(),
            arguments: serde_json::json!({"command":"echo forbidden","session_id":null,"workdir":null,
                "additional_read_roots":null,"additional_write_roots":null,"yield_time_ms":null,"max_chars":null}),
        }, &readonly_context).await.is_err());
        assert!(
            app.execute_tool(
                &crate::model_protocol::AgentToolCall {
                    id: "select-apps".into(),
                    name: "create".into(),
                    arguments: serde_json::json!({
                        "stage_id":null,"name":"Extra","workspace":null,"read_only":false,
                        "instructions":null,"model":null,"apps":["files"]
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
    async fn failed_workspace_preparation_does_not_change_actor_configuration() {
        let (directory, system) = fixture().await;
        let old = system.actor(FIXTURE_ACTOR_ID).unwrap().config;
        let blocked = directory.path().join("existing-file");
        std::fs::write(&blocked, "not a directory").unwrap();
        let mut proposed = old.clone();
        proposed.workspace = blocked.join("child");
        proposed.permissions.readable_roots = vec![proposed.workspace.clone()];
        proposed.permissions.writable_roots = vec![proposed.workspace.clone()];
        assert!(
            system
                .update_owned(FIXTURE_ACTOR_ID, proposed)
                .await
                .is_err()
        );
        assert_eq!(system.actor(FIXTURE_ACTOR_ID).unwrap().config, old);
        assert!(!system.inner.reconfiguring.load(Ordering::Acquire));
    }
}
