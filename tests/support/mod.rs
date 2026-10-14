//! Isolated actual HTTP servers and OPAQUE accounts; never contacts real models.
use daat_locus::{
    accounts::AccountStore,
    actors::{
        ActorConfig, ActorPermissions, ActorSystem, ApprovalDecision, ApprovalMode, ApprovalRecord,
        ModelFactory, ROOT_ACTOR_ID, ToolPermissionRequest,
    },
    config::{Config, ModelConfig, ProviderConfig},
    core::ModelProvider,
    service::{ServiceState, router},
};
use miette::Result;
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use std::sync::Arc;

struct NoModel;

impl ModelFactory for NoModel {
    fn build(&self, _: &str, _: &Config) -> Result<Arc<dyn ModelProvider>> {
        panic!("paused HTTP test must not construct a real provider")
    }
}
pub struct Fixture {
    pub stage: Arc<daat_locus::stage::runtime::Runtime>,
    pub _temp: tempfile::TempDir,
    pub system: ActorSystem,
    pub url: String,
    pub cookie: parking_lot::RwLock<String>,
    pub password: parking_lot::RwLock<String>,
    pub auth: AccountStore,
    pub client: Client,
    pub server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    pub async fn login_cookie(&self) -> String {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
        let password = self.password.read().clone();
        let client = daat_locus_auth::LoginClient::start(password.into_bytes()).unwrap();
        let reply = self
            .client
            .post(format!("{}/v1/account/login/start", self.url))
            .header("origin", &self.url)
            .header("x-daat-request", "1")
            .json(&json!({"name":"fixture","request":B64.encode(client.request())}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            reply.status(),
            StatusCode::OK,
            "{}",
            reply.text().await.unwrap()
        );
        let reply: Value = reply.json().await.unwrap();
        let proof = client
            .finish(
                &B64.decode(reply["response"].as_str().unwrap()).unwrap(),
                &B64.decode(reply["context"].as_str().unwrap()).unwrap(),
            )
            .unwrap();
        let response = self
            .client
            .post(format!("{}/v1/account/login/finish", self.url))
            .header("origin", &self.url)
            .header("x-daat-request", "1")
            .json(&json!({"id":reply["id"],"message":B64.encode(proof)}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let cookie = response.headers()["set-cookie"].to_str().unwrap();
        assert!(
            cookie.contains("HttpOnly")
                && cookie.contains("SameSite=Strict")
                && cookie.contains("Path=/v1")
        );
        let cookie = cookie.split(';').next().unwrap().to_owned();
        *self.cookie.write() = cookie.clone();
        cookie
    }
    pub async fn change_password(&self, password: &str) {
        use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD as B64};
        let client =
            daat_locus_auth::RegistrationClient::start(password.as_bytes().to_vec()).unwrap();
        let response = self
            .owner(reqwest::Method::POST, "/v1/account/registration/start")
            .json(&json!({"name":"fixture","request":B64.encode(client.request())}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let response: Value = response.json().await.unwrap();
        let upload = client
            .finish(&B64.decode(response["response"].as_str().unwrap()).unwrap())
            .unwrap();
        assert_eq!(
            self.owner(reqwest::Method::POST, "/v1/account/registration/finish")
                .json(&json!({"id":response["id"],"message":B64.encode(upload)}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
        *self.password.write() = password.to_owned();
    }
    pub async fn new() -> Self {
        let mut config = Config {
            main_model: "mock".into(),
            ..Default::default()
        };
        config.providers.insert(
            "mock".into(),
            ProviderConfig::OpenaiCompatible {
                base_url: "http://model.invalid/v1".into(),
                api_key: "fixture-secret-key".into(),
            },
        );
        config.models.insert(
            "mock".into(),
            ModelConfig {
                provider: "mock".into(),
                model_id: "mock".into(),
                ..Default::default()
            },
        );
        Self::with_config(config).await
    }
    pub async fn with_config(config: Config) -> Self {
        Self::with_config_and_account(config, true).await
    }
    pub async fn with_config_and_account(config: Config, initialize_account: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let system = ActorSystem::open_paused_with_factory(
            temp.path().join("actors"),
            workspace,
            config,
            Arc::new(NoModel),
        )
        .await
        .unwrap();
        let mut root_config = system.actor(ROOT_ACTOR_ID).unwrap().config;
        root_config.permissions.approval_mode = ApprovalMode::None;
        system
            .update_owned(ROOT_ACTOR_ID, root_config)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let state = ServiceState::new(system.clone(), url.clone())
            .unwrap()
            .with_listen_address(listener.local_addr().unwrap());
        let stage = state.stage.clone().unwrap();
        let auth = stage.host.accounts.clone();
        if initialize_account {
            stage.host.initialize(true).unwrap();
            let setup = daat_locus_auth::new_setup();
            let group = stage.host.account_id().unwrap();
            let client =
                daat_locus_auth::RegistrationClient::start(b"isolated fixture password".to_vec())
                    .unwrap();
            let response = daat_locus_auth::registration_response(
                &setup,
                format!("DaatLocus.Account.v1/{group}/fixture").as_bytes(),
                &client.request(),
            )
            .unwrap();
            let record =
                daat_locus_auth::password_file(&client.finish(&response).unwrap()).unwrap();
            let bundle = daat_locus::accounts::CredentialBundle::new(
                group.clone(),
                "fixture".into(),
                &setup,
                &record,
                None,
            )
            .unwrap();
            auth.install(bundle, &group).unwrap();
        }
        let app = router(state);
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let fixture = Self {
            stage,
            _temp: temp,
            system,
            url,
            auth,
            client: Client::new(),
            server,
            cookie: parking_lot::RwLock::new(String::new()),
            password: parking_lot::RwLock::new("isolated fixture password".into()),
        };
        if initialize_account {
            fixture.login_cookie().await;
        }
        fixture
    }
    pub fn owner(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.client
            .request(method, format!("{}{path}", self.url))
            .header("cookie", self.cookie.read().clone())
            .header("origin", &self.url)
            .header("x-daat-request", "1")
    }
    pub async fn child(&self, name: &str) -> String {
        let workspace = self
            .system
            .actor(ROOT_ACTOR_ID)
            .unwrap()
            .config
            .workspace
            .join(name);
        std::fs::create_dir_all(&workspace).unwrap();
        let mut config = ActorConfig {
            name: name.into(),
            workspace: workspace.clone(),
            apps: vec!["files".into()],
            model: None,
            instructions: String::new(),
            permissions: ActorPermissions::workspace(workspace, false),
        };
        config.permissions.approval_mode = ApprovalMode::None;
        self.system.create_owned(ROOT_ACTOR_ID, config).unwrap().id
    }

    pub async fn approval(&self, actor: &str, mode: ApprovalMode) -> ApprovalRecord {
        let mut config = self.system.actor(actor).unwrap().config;
        config.permissions.approval_mode = mode;
        self.system
            .update_owned(actor, config.clone())
            .await
            .unwrap();
        self.system
            .send_user(actor, "permission test".into(), vec![], None)
            .unwrap();
        let message = self.system.store().claim_next(actor).unwrap().unwrap();
        self.system.store().create_approval(ApprovalRecord {
            seq: 0, id: uuid::Uuid::new_v4().to_string(), actor_id: actor.into(), message_id: message.id,
            call: daat_locus::model_protocol::AgentToolCall { id: uuid::Uuid::new_v4().to_string(), name: "files__write_file".into(), arguments: json!({"path": "proposal.txt", "content": "fixture-secret-key", "expected_sha256": null}) },
            requested: ToolPermissionRequest { readable_roots: vec![config.workspace.clone()], writable_roots: vec![config.workspace.join("extra")], ..Default::default() },
            mode, decision: ApprovalDecision::Pending, reason: "Owner approval required".into(), created_at_ms: chrono::Utc::now().timestamp_millis(), resolved_at_ms: None,
        }).unwrap()
    }
}
pub fn fixture_enrollment(
    host: &daat_locus::stage::host::Host,
    request: &daat_locus::stage::identity::JoinRequest,
    can_enroll: bool,
    grants: Vec<daat_locus::stage::ledger::ActorGrant>,
) -> daat_locus::stage::Result<daat_locus::stage::identity::Enrollment> {
    let now = chrono::Utc::now().timestamp_millis();
    let own = host.enrollment()?;
    let issuer = daat_locus::stage::identity::verify_enrollment(
        &own.root,
        &own.chain,
        now,
        &Default::default(),
    )?;
    let enrollment = host.identity()?.authorize(
        &own,
        request,
        now,
        issuer.expires_at_ms.min(now + 180 * 24 * 60 * 60 * 1000),
        can_enroll,
    )?;
    daat_locus::stage::ledger::StageLedger::attach(host.system.store(), host.local).import_peer(
        enrollment.clone(),
        grants,
        now,
    )?;
    Ok(enrollment)
}
