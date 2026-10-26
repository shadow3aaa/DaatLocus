use super::*;
use crate::app::{App, AppId};

struct WaitingApp(Arc<tokio::sync::Notify>);
#[async_trait::async_trait]
impl App for WaitingApp {
    fn id(&self) -> AppId {
        AppId::new("waiting_app").unwrap()
    }
    fn managed_revision(&self) -> Option<u64> {
        Some(1)
    }
    async fn prepare_turn(&mut self, _: &AppToolExecutionContext, _: bool) -> Result<()> {
        self.0.notify_one();
        std::future::pending().await
    }
}

#[tokio::test]
async fn stop_and_exact_input_cancel_interrupt_unbounded_app_preparation() {
    let directory = tempfile::tempdir().unwrap();
    let system = ActorSystem::open_account_gated(
        directory.path().join("state"),
        directory.path().join("work"),
        crate::config::Config::default(),
    )
    .await
    .unwrap();
    for stop_actor in [false, true] {
        let mut actor = system.core_device_actor().unwrap();
        actor.id = "waiting-actor".into();
        let context = system.owner_app_context().unwrap();
        let message = MessageRecord {
            seq: 0,
            id: uuid::Uuid::new_v4().to_string(),
            actor_id: actor.id.clone(),
            source: super::super::MessageSource::User,
            reply_to: super::super::ReplyTarget::User,
            kind: super::super::MessageKind::Request,
            in_reply_to: None,
            remote_authority: None,
            content: "Wait for the server".into(),
            attachments: vec![],
            status: MessageStatus::Running,
            cancel_requested: false,
            started_at_ms: Some(0),
            finished_at_ms: None,
            final_generation: None,
            reply: None,
            error: None,
            created_at_ms: 0,
            updated_at_ms: 0,
        };
        let control = ActorControl::default();
        let active = control.clone();
        let input_id = message.id.clone();
        let entered = Arc::new(tokio::sync::Notify::new());
        let ready = entered.clone();
        let mut apps = AppManager::new(vec![Box::new(WaitingApp(entered))]).unwrap();
        let running = system.clone();
        let preparation = tokio::spawn(async move {
            refresh_apps(
                &running, &actor, &message, &active, &mut apps, &context, true,
            )
            .await
        });
        ready.notified().await;
        assert!(!preparation.is_finished());
        if stop_actor {
            control.stop.store(true, Ordering::Release);
        } else {
            control.cancelled.lock().insert(input_id);
        }
        let result = tokio::time::timeout(Duration::from_secs(1), preparation)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_err(),
            "preparation must stop without waiting for a server deadline"
        );
    }
    system.shutdown().await;
}
