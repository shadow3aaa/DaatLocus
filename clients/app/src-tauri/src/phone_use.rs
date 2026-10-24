//! Android-only JNI adapter. The core sees typed PhoneUse results, never Java or shell access.
use daat_locus::computer_use_app::android::{self, Backend, Reply};
use jni::{
    jni_sig, jni_str,
    objects::{JByteArray, JClass, JObject, JString},
    refs::Global,
};
use std::sync::Arc;

struct Bridge {
    class: Global<JClass<'static>>,
}
impl Bridge {
    fn bytes(&self, method: &str, argument: &str) -> miette::Result<Vec<u8>> {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<Vec<u8>> {
            let argument = env.new_string(argument)?;
            let bytes = env.call_static_method(
                &self.class,
                jni::strings::JNIString::new(method),
                jni_sig!("(Ljava/lang/String;)[B"),
                &[(&argument).into()],
            );
            if bytes.is_err() && env.exception_check() {
                env.exception_clear();
            }
            let bytes = bytes?.l()?;
            let bytes = env.cast_local::<JByteArray>(bytes)?;
            env.convert_byte_array(&bytes)
        })
        .map_err(|_| miette::miette!("PhoneUse display image unavailable"))
    }
    fn invoke(&self, method: &str, argument: Option<&str>) -> Result<String, String> {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<String> {
            let argument = argument.map(|value| env.new_string(value)).transpose()?;
            let args = argument
                .as_ref()
                .map(|value| vec![value.into()])
                .unwrap_or_default();
            let signature = if argument.is_some() {
                jni_sig!("(Ljava/lang/String;)Ljava/lang/String;")
            } else {
                jni_sig!("()Ljava/lang/String;")
            };
            let value = env.call_static_method(
                &self.class,
                jni::strings::JNIString::new(method),
                signature,
                &args,
            );
            if value.is_err() && env.exception_check() {
                env.exception_clear();
            }
            let value = value?.l()?;
            env.cast_local::<JString>(value)?.try_to_string(env)
        })
        .map_err(|_| "PhoneUse native operation unavailable".into())
    }
}
impl Backend for Bridge {
    fn preview_catalog(&self) -> miette::Result<daat_locus::computer_use_app::preview::Catalog> {
        serde_json::from_str(
            &self
                .invoke("virtualDisplays", None)
                .map_err(|e| miette::miette!("{e}"))?,
        )
        .map_err(|_| miette::miette!("Invalid virtual display catalog"))
    }
    fn preview_frame(&self, id: &str) -> miette::Result<Option<Vec<u8>>> {
        let bytes = self.bytes("virtualDisplayImage", id)?;
        Ok((!bytes.is_empty()).then_some(bytes))
    }
    fn reset(&self) -> miette::Result<()> {
        self.invoke("reset", None)
            .map(|_| ())
            .map_err(|e| miette::miette!("{e}"))
    }
    fn call(&self, request: &str) -> miette::Result<Reply> {
        let value = self
            .invoke("request", Some(request))
            .map_err(|e| miette::miette!("{e}"))?;
        let value: serde_json::Value = serde_json::from_str(&value)
            .map_err(|_| miette::miette!("Invalid native PhoneUse response"))?;
        let request: serde_json::Value =
            serde_json::from_str(request).map_err(|e| miette::miette!("{e}"))?;
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        let image = vm
            .attach_current_thread(|env| -> jni::errors::Result<Vec<u8>> {
                let id = env.new_string(request["id"].as_str().unwrap_or_default())?;
                let bytes = env
                    .call_static_method(
                        &self.class,
                        jni_str!("image"),
                        jni_sig!("(Ljava/lang/String;)[B"),
                        &[(&id).into()],
                    )?
                    .l()?;
                let bytes = env.cast_local::<JByteArray>(bytes)?;
                env.convert_byte_array(&bytes)
            })
            .map_err(|_| {
                miette::miette!("PhoneUse screenshot unavailable after operation; do not replay")
            })?;
        Ok(Reply {
            summary: value["summary"].as_str().unwrap_or("PhoneUse").into(),
            payload: value["payload"].clone(),
            failure: value["failure"].as_str().map(str::to_owned),
            image: (!image.is_empty()).then_some(image),
        })
    }
    fn scope(&self) -> miette::Result<serde_json::Value> {
        serde_json::from_str(
            &self
                .invoke("scope", None)
                .map_err(|e| miette::miette!("{e}"))?,
        )
        .map_err(|e| miette::miette!("{e}"))
    }
    fn cancel(&self, request: &str) {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        let _ = vm.attach_current_thread(|env| -> jni::errors::Result<()> {
            let id = env.new_string(request)?;
            env.call_static_method(
                &self.class,
                jni_str!("cancel"),
                jni_sig!("(Ljava/lang/String;)V"),
                &[(&id).into()],
            )?;
            Ok(())
        });
    }
    fn has_resources(&self, session: &str) -> bool {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        vm.attach_current_thread(|env| -> jni::errors::Result<bool> {
            let session = env.new_string(session)?;
            env.call_static_method(
                &self.class,
                jni_str!("hasResources"),
                jni_sig!("(Ljava/lang/String;)Z"),
                &[(&session).into()],
            )?
            .z()
        })
        .unwrap_or(true)
    }
}
static BRIDGE: std::sync::OnceLock<Arc<Bridge>> = std::sync::OnceLock::new();
pub fn initialize() -> Result<(), Box<dyn std::error::Error>> {
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
        let name = env.new_string("io.shadow3aaa.daatlocus.PhoneUseController")?;
        let class = env
            .call_method(
                &loader,
                jni_str!("loadClass"),
                jni_sig!("(Ljava/lang/String;)Ljava/lang/Class;"),
                &[(&name).into()],
            )?
            .l()?;
        let class = env.cast_local::<JClass>(class)?;
        env.new_global_ref(&class)
    })?;
    let bridge = Arc::new(Bridge { class });
    BRIDGE
        .set(bridge.clone())
        .map_err(|_| "PhoneUse already initialized")?;
    android::install_backend(bridge).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn phone_use_status(window: tauri::Webview) -> Result<serde_json::Value, String> {
    crate::transport::authorize_owner(&window).await?;
    let bridge = BRIDGE.get().ok_or("PhoneUse unavailable")?.clone();
    tokio::task::spawn_blocking(move || {
        serde_json::from_str(&bridge.invoke("status", None)?)
            .map_err(|_| "Invalid PhoneUse status".into())
    })
    .await
    .map_err(|_| "PhoneUse status unavailable")?
}
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayPatch {
    width: Option<i32>,
    height: Option<i32>,
    density: Option<i32>,
}

#[tauri::command]
pub async fn phone_use_display_configure(
    window: tauri::Webview,
    patch: DisplayPatch,
) -> Result<serde_json::Value, String> {
    crate::transport::authorize_owner(&window).await?;
    let bridge = BRIDGE.get().ok_or("PhoneUse unavailable")?.clone();
    let patch = serde_json::to_string(&patch).map_err(|_| "Invalid display settings")?;
    tokio::task::spawn_blocking(move || {
        serde_json::from_str(&bridge.invoke("configureDisplay", Some(&patch))?)
            .map_err(|_| "Invalid PhoneUse status".into())
    })
    .await
    .map_err(|_| "Display configuration interrupted")?
}
#[tauri::command]
pub async fn phone_use_configure(
    window: tauri::Webview,
    enabled: bool,
    method: String,
) -> Result<serde_json::Value, String> {
    crate::transport::authorize_owner(&window).await?;
    if !matches!(method.as_str(), "root" | "shizuku") {
        return Err("Invalid authorization method".into());
    }
    let bridge = BRIDGE.get().ok_or("PhoneUse unavailable")?.clone();
    tokio::task::spawn_blocking(move || {
        let context = ndk_context::android_context();
        let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
        let value = vm
            .attach_current_thread(|env| -> jni::errors::Result<String> {
                let method = env.new_string(method)?;
                let reply = env
                    .call_static_method(
                        &bridge.class,
                        jni_str!("configure"),
                        jni_sig!("(ZLjava/lang/String;)Ljava/lang/String;"),
                        &[enabled.into(), (&method).into()],
                    )?
                    .l()?;
                env.cast_local::<JString>(reply)?.try_to_string(env)
            })
            .map_err(|_| "PhoneUse configuration failed")?;
        serde_json::from_str(&value).map_err(|_| "Invalid PhoneUse status".into())
    })
    .await
    .map_err(|_| "PhoneUse configuration interrupted")?
}
