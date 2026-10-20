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
    pub fn connectivity_changed(&self) {
        if let Some(stage) = &self.state.stage {
            stage.connectivity_changed();
        }
    }
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
            // Local inference is independent of discovery/public connectivity.
            // A failed transport restart must not leave Android suspended.
            self.state.system.resume_execution().await?;
            if let Some(stage) = &self.state.stage {
                stage.connectivity_changed();
                if let Err(error) = stage.start().await {
                    tracing::warn!(reason = %error, "Device transport could not resume");
                }
            }
            Ok(())
        } else {
            self.state.system.suspend_execution().await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn first_model_and_foreground_resume_enable_local_core_without_restart() {
        let directory = tempfile::tempdir().unwrap();
        let system = ActorSystem::open_account_gated(
            directory.path().join("actors"),
            directory.path().join("workspace"),
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let state = ServiceState::new(system.clone(), "http://127.0.0.1:12345".into()).unwrap();
        let stage = state.stage.as_ref().unwrap().clone();
        let (_, logging) = tracing_appender::non_blocking(std::io::sink());
        let service = MobileService {
            _logging: logging,
            origin: "http://127.0.0.1:12345".into(),
            state,
            lifecycle: Mutex::new(()),
            foreground: AtomicBool::new(true),
        };
        let mut settings = stage.host.settings();
        settings.listen = "127.0.0.1:0".into();
        stage.host.save_settings(settings).unwrap();
        stage.start().await.unwrap();
        stage.host.initialize(true).unwrap();
        stage.start().await.unwrap();
        system.activate_account_workers().await.unwrap();
        let core = stage.core.read().clone().unwrap();
        assert_eq!(core.status()["state"], "model_required");
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while core.status()["synchronized"] != true {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await
        .unwrap();
        service.set_foreground(false);
        service.apply_foreground().await.unwrap();
        assert!(!system.workers_enabled());
        let mut config = system.config();
        config.main_model = "local-test".into();
        config.providers.insert(
            "local-test".into(),
            crate::config::ProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:1/v1".into(),
                api_key: "test".into(),
            },
        );
        config.models.insert(
            "local-test".into(),
            crate::config::ModelConfig {
                provider: "local-test".into(),
                model_id: "test".into(),
                ..Default::default()
            },
        );
        let persistent = config.clone();
        system
            .update_config_with(config, async {
                crate::accounts::configuration::persist_device_config(&system, &persistent)?;
                stage.host.configurations.publish(&stage.host, &persistent)
            })
            .await
            .unwrap();
        assert!(
            !system.workers_enabled(),
            "saving models must respect suspension"
        );
        assert_eq!(core.status()["model_ready"], true);
        service.set_foreground(true);
        service.apply_foreground().await.unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), async {
            while !core.accepts_input() {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        })
        .await;
        let status = core.status();
        assert!(stage.host.settings().ssh.is_empty());
        assert!(stage.host.settings().signaling_origin.is_none());
        stage.stop().await;
        system.shutdown().await;
        assert!(
            result.is_ok(),
            "first model left local execution unavailable: {status}"
        );
    }

    #[tokio::test]
    async fn foreground_resumes_execution_when_device_transport_cannot_start() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let system = ActorSystem::open(
            directory.path().join("actors"),
            workspace,
            crate::config::Config::default(),
        )
        .await
        .unwrap();
        let state = ServiceState::new(system.clone(), "http://127.0.0.1:12345".into()).unwrap();
        let stage = state.stage.as_ref().unwrap().clone();
        stage.host.initialize(true).unwrap();
        // Hold the device listener port so the actual startup fails, rather
        // than mocking the lifecycle result or requiring an external network.
        let occupied = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut settings = stage.host.settings();
        settings.listen = occupied.local_addr().unwrap().to_string();
        stage.host.save_settings(settings).unwrap();
        let (_, logging) = tracing_appender::non_blocking(std::io::sink());
        let service = MobileService {
            _logging: logging,
            origin: "http://127.0.0.1:12345".into(),
            state,
            lifecycle: Mutex::new(()),
            foreground: AtomicBool::new(false),
        };
        service.apply_foreground().await.unwrap();
        assert!(!system.workers_enabled());
        service.set_foreground(true);
        let resumed = service.apply_foreground().await;
        let enabled = system.workers_enabled();
        let transport_failed = stage.core.read().is_none();
        stage.stop().await;
        system.shutdown().await;
        assert!(
            transport_failed,
            "occupied device listener did not fail startup"
        );
        assert!(resumed.is_ok(), "foreground transition failed: {resumed:?}");
        assert!(enabled, "transport failure left local execution suspended");
    }
}
