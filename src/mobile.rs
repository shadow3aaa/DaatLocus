//! In-process Android service. Reuses the Actor, account and HTTP contracts;
//! no maintenance daemon, desktop process launcher or shell is started.
use crate::{actors::ActorSystem, service::ServiceState};
use miette::{IntoDiagnostic, Result};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::Mutex;

pub struct MobileService {
    _logging: tracing_appender::non_blocking::WorkerGuard,
    pub origin: String,
    state: ServiceState,
    lifecycle: Mutex<()>,
    foreground: AtomicBool,
}
impl MobileService {
    pub async fn start(application_dir: PathBuf) -> Result<Arc<Self>> {
        // Workspace is a sibling of private service storage. It must never be
        // inside the credential deny-root used by every App execution context.
        let root = application_dir.join("service");
        crate::daat_locus_paths::initialize_native_root(root.clone())
            .map_err(|_| miette::miette!("Application storage was already initialized"))?;
        std::fs::create_dir_all(&root).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(&root)?;
        let logging =
            crate::logging::init_logging(&crate::daat_locus_paths::daat_locus_paths_sync()).await;
        let workspace = application_dir.join("workspace");
        std::fs::create_dir_all(&workspace).into_diagnostic()?;
        crate::actors::permissions::protect_private_path(&workspace)?;
        let config = if crate::config_setup::ensure_config_readiness()
            .await
            .is_complete()
        {
            crate::config_setup::load_complete_config().await?
        } else {
            crate::config::Config::default()
        };
        let system =
            ActorSystem::open_account_gated(root.join("actors"), workspace, config).await?;
        let port_file = root.join("runtime/mobile-port");
        let port = std::fs::read_to_string(&port_file)
            .ok()
            .and_then(|text| text.trim().parse::<u16>().ok())
            .unwrap_or(0);
        let listener =
            match tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port))).await {
                Ok(listener) => listener,
                Err(_) => tokio::net::TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
                    .await
                    .into_diagnostic()?,
            };
        let address = listener.local_addr().into_diagnostic()?;
        crate::persistence::write_bytes_atomic_sync(
            &port_file,
            address.port().to_string().as_bytes(),
            crate::persistence::PersistenceFileMode::Private,
        )
        .into_diagnostic()?;
        let origin = format!("http://{address}");
        let state = ServiceState::new(system.clone(), origin.clone())?.with_listen_address(address);
        let stage = state.stage.as_ref().expect("account runtime");
        if let Some(record) = stage.host.accounts.current()? {
            if stage.host.account_id().as_deref() != Some(&record.account_id) {
                return Err(miette::miette!("Account storage namespace mismatch"));
            }
            if let Some(definitions) = stage.host.configurations.current()? {
                system
                    .update_config(
                        crate::accounts::configuration::ConfigurationStore::candidate(
                            &stage.host,
                            &definitions,
                        )?,
                    )
                    .await?;
            }
            system.activate_account_workers().await?;
        }
        // A connectivity failure must not hide the login or persisted conversation.
        if stage.start().await.is_err() {
            tracing::warn!("Device transport could not start");
        }
        let router = crate::service::router(state.clone());
        tokio::spawn(async move {
            if axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .is_err()
            {
                tracing::error!("Mobile HTTP service stopped");
            }
        });
        Ok(Arc::new(Self {
            _logging: logging,
            origin,
            state,
            lifecycle: Mutex::new(()),
            foreground: AtomicBool::new(true),
        }))
    }
    /// Called only by the native main WebView after exact-origin verification.
    pub async fn bootstrap(
        &self,
        input: daat_locus_client_protocol::BootstrapRequest,
    ) -> Result<daat_locus_client_protocol::BootstrapReply> {
        crate::service::native_bootstrap(self.state.clone(), &self.origin, input).await
    }
    pub fn set_foreground(&self, active: bool) {
        self.foreground.store(active, Ordering::Release);
    }
    pub async fn apply_foreground(&self) -> Result<()> {
        let _guard = self.lifecycle.lock().await;
        if self.foreground.load(Ordering::Acquire) {
            if let Some(stage) = &self.state.stage {
                stage
                    .start()
                    .await
                    .map_err(|_| miette::miette!("Device transport unavailable"))?;
            }
            self.state.system.resume_execution().await
        } else {
            self.state.system.suspend_execution().await
        }
    }
}
