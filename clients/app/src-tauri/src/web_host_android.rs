//! In-process adapter to an isolated Android WebView; no ADB or debug endpoint.
use daat_locus_client_protocol::web::{
    BROWSER_CURSOR_SCRIPT, BROWSER_METRICS_SCRIPT, CAPTURE_SCRIPT, Reply, Request,
};
use jni::{
    jni_sig, jni_str,
    objects::{JClass, JObject, JString},
    refs::Global,
};
use std::sync::Arc;

struct Bridge {
    class: Global<JClass<'static>>,
}
#[tauri::command]
pub async fn browser_control(
    window: tauri::Webview,
    action: String,
    tab: Option<String>,
    url: Option<String>,
    bounds: Option<daat_locus_client_protocol::web::BrowserBounds>,
    foreground: Option<Vec<daat_locus_client_protocol::web::BrowserHitRegion>>,
) -> Result<serde_json::Value, String> {
    crate::transport::authorize(&window)?;
    if action != "detach" {
        crate::transport::authorize_owner(&window).await?;
    }
    if !matches!(
        action.as_str(),
        "preferences"
            | "set_preferences"
            | "mount"
            | "detach"
            | "list"
            | "select"
            | "move_tab"
            | "open"
            | "navigate"
            | "back"
            | "forward"
            | "reload"
            | "close"
            | "new_tab"
            | "library"
            | "clear_history"
            | "remove_history"
            | "save_download"
            | "remove_download"
            | "zoom"
            | "print"
            | "find"
            | "find_next"
            | "find_previous"
            | "find_close"
            | "screenshot"
            | "clear_data"
            | "stop"
    ) || bounds.as_ref().is_some_and(|bounds| !bounds.validate())
        || foreground
            .as_ref()
            .is_some_and(|cuts| cuts.len() > 512 || cuts.iter().any(|cut| !cut.validate()))
    {
        return Err("Invalid browser control".into());
    }
    let input=serde_json::json!({"profile":daat_locus_client_protocol::web::SHARED_BROWSER_PROFILE,"action":action,"tab":tab,"url":url,"bounds":bounds,"foreground":foreground,"background_script":include_str!("browser-background.js")}).to_string();
    tokio::task::spawn_blocking(move || {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<String> {
            let activity = unsafe { JObject::from_raw(env, context.context().cast()) };
            let loader = env
                .call_method(
                    &activity,
                    jni_str!("getClassLoader"),
                    jni_sig!("()Ljava/lang/ClassLoader;"),
                    &[],
                )?
                .l()?;
            let name = env.new_string("io.shadow3aaa.daatlocus.WebHost")?;
            let class = env
                .call_method(
                    &loader,
                    jni_str!("loadClass"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                    &[(&name).into()],
                )?
                .l()?;
            let class = env.cast_local::<JClass>(class)?;
            let profile = env.new_string(&input)?;
            let value = env
                .call_static_method(
                    &class,
                    jni_str!("browserControl"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                    &[(&profile).into()],
                )?
                .l()?;
            env.cast_local::<JString>(value)?.try_to_string(env)
        })
        .map_err(|_| "Android browser unavailable".to_string())
        .and_then(|value| {
            serde_json::from_str::<serde_json::Value>(&value)
                .map_err(|_| "Invalid Android browser reply".into())
        })
        .and_then(|value| {
            if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
                Err(error.to_owned())
            } else {
                Ok(value)
            }
        })
    })
    .await
    .map_err(|_| "Android browser unavailable")?
}
impl Bridge {
    fn execute(&self, request: &Request) -> Result<Reply, String> {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        let json = serde_json::to_string(request).map_err(|_| "Invalid Web host command")?;
        let result = vm
            .attach_current_thread(|env| -> jni::errors::Result<String> {
                let request = env.new_string(&json)?;
                let script = env.new_string(CAPTURE_SCRIPT)?;
                let metrics = env.new_string(BROWSER_METRICS_SCRIPT)?;
                let cursor=env.new_string(BROWSER_CURSOR_SCRIPT)?;
                let result = env.call_static_method(
                    &self.class,
                    jni_str!("request"),
                    jni_sig!(
                        "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;"
                    ),
                    &[(&request).into(), (&script).into(), (&metrics).into(),(&cursor).into()],
                );
                if result.is_err() && env.exception_check() {
                    env.exception_clear();
                }
                let value = result?.l()?;
                env.cast_local::<JString>(value)?.try_to_string(env)
            })
            .map_err(|_| "Android Web host unavailable")?;
        if result.len() > 12 * 1024 * 1024 {
            return Err("Android Web capture exceeds its limit".into());
        }
        serde_json::from_str(&result).map_err(|_| "Invalid Android Web capture".into())
    }
}
pub fn start(
    host: Arc<daat_locus::web_host::Host>,
    origin: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
    let class = vm.attach_current_thread(|env| -> jni::errors::Result<_> {
        let object = unsafe { JObject::from_raw(env, context.context().cast()) };
        let loader = env
            .call_method(
                &object,
                jni_str!("getClassLoader"),
                jni_sig!("()Ljava/lang/ClassLoader;"),
                &[],
            )?
            .l()?;
        let name = env.new_string("io.shadow3aaa.daatlocus.WebHost")?;
        let class = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[(&name).into()],
            )?
            .l()?;
        let class = env.cast_local::<JClass>(class)?;
        let origin = env.new_string(origin)?;
        env.call_static_method(
            &class,
            jni_str!("setServiceOrigin"),
            jni_sig!("(Ljava/lang/String;)V"),
            &[(&origin).into()],
        )?;
        env.new_global_ref(&class)
    })?;
    let bridge = Arc::new(Bridge { class });
    let mut connection = host.attach().map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn(async move {
        while let Some(pending) = connection.requests.recv().await {
            if pending.reply.is_closed() {
                continue;
            }
            let request = pending.request.clone();
            let bridge = bridge.clone();
            let result = tokio::task::spawn_blocking(move || bridge.execute(&request))
                .await
                .map_err(|_| {
                    miette::miette!("Android Web host interrupted; do not replay navigation")
                })
                .and_then(|result| result.map_err(|error| miette::miette!("{error}")))
                .and_then(|reply| daat_locus::web_host::resolve(&pending.request, reply));
            let _ = pending.reply.send(result);
        }
    });
    Ok(())
}
