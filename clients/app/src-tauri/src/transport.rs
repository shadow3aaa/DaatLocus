//! A profile-bound HTTP transport. Business authorization remains in the service.
use reqwest::{Client, Method, Response, header::HeaderMap};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tauri::{
    Manager, WebviewWindow,
    ipc::{InvokeBody, Request},
};
use tokio::sync::{Mutex as AsyncMutex, watch};
use url::Url;

type Result<T> = std::result::Result<T, String>;
const UNAVAILABLE: &str = "Native service request unavailable";
const MAX_CHUNK: usize = 256 * 1024;
const NATIVE_UI_MARKER: &str = if cfg!(feature = "custom-protocol") {
    "DaatLocus.NativeWebUI.v1"
} else {
    "DaatLocus.NativeDevelopment.v1"
};

pub struct Transport {
    origin: Arc<RwLock<Url>>,
    client: Client,
    next: AtomicU64,
    generation: AtomicU64,
    requests: Mutex<HashMap<u64, Arc<Pending>>>,
    resources: tokio::sync::Semaphore,
    downloads: tokio::sync::Semaphore,
}
struct Pending {
    generation: u64,
    cancel: watch::Sender<bool>,
    state: AsyncMutex<RequestState>,
    touched: Mutex<Instant>,
}
enum RequestState {
    Ready(reqwest::RequestBuilder, Vec<u8>),
    Reading(Response, bytes::Bytes),
    Closed,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Metadata {
    path: String,
    method: String,
    headers: Vec<(String, String)>,
}
#[derive(Serialize)]
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
}

pub fn app_url(url: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && ((url.scheme() == "tauri" && url.host_str() == Some("localhost"))
            || matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("tauri.localhost"))
}
pub fn authorize(window: &WebviewWindow) -> Result<()> {
    let url = window.url().map_err(|_| UNAVAILABLE)?;
    if window.label() != "main" || !navigation(window.app_handle(), &url) {
        return Err(UNAVAILABLE.into());
    }
    Ok(())
}
pub fn navigation(app: &tauri::AppHandle, url: &Url) -> bool {
    app_url(url)
        || cfg!(debug_assertions)
            && app.config().build.dev_url.as_ref().is_some_and(|dev| {
                dev.origin() == url.origin()
                    && url.username().is_empty()
                    && url.password().is_none()
            })
}
pub fn resource_url(url: &Url) -> bool {
    url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && ((url.scheme() == "dl-resource" && url.host_str() == Some("localhost"))
            || matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("dl-resource.localhost"))
        && resource_path(url.path())
        && !url.path().contains(['%', '\\'])
}
fn endpoint(origin: &Url, path: &str) -> Result<Url> {
    let raw_path = path.split('?').next().unwrap_or_default();
    if !raw_path.starts_with("/v1/")
        || raw_path.contains(['%', '\\', '#'])
        || path.contains('#')
        || raw_path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(UNAVAILABLE.into());
    }
    let url = origin.join(path).map_err(|_| UNAVAILABLE)?;
    if url.origin() != origin.origin() {
        return Err(UNAVAILABLE.into());
    }
    Ok(url)
}
fn response_headers(headers: &HeaderMap) -> Vec<(String, String)> {
    // In particular, never expose Set-Cookie to JavaScript or active resources.
    [
        "content-type",
        "content-length",
        "cache-control",
        "content-disposition",
        "content-security-policy",
        "x-content-type-options",
        "referrer-policy",
        "retry-after",
        "etag",
        "accept-ranges",
        "content-range",
    ]
    .into_iter()
    .filter_map(|name| {
        headers
            .get(name)?
            .to_str()
            .ok()
            .map(|v| (name.into(), v.into()))
    })
    .collect()
}
impl Transport {
    pub fn new(origin: Arc<RwLock<Url>>) -> Result<Self> {
        // Workspace feature unification can enable reqwest's rustls-no-provider
        // in the desktop binary too, which does not link the service initializer.
        #[cfg(any(target_os = "windows", target_os = "macos"))]
        let _ = rustls::crypto::ring::default_provider().install_default();
        #[cfg(target_os = "android")]
        let builder = daat_locus::http_client::builder();
        #[cfg(not(target_os = "android"))]
        let builder = Client::builder();
        Ok(Self {
            origin,
            client: builder
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(5))
                .build()
                .map_err(|_| UNAVAILABLE)?,
            next: AtomicU64::new(1),
            generation: AtomicU64::new(0),
            requests: Mutex::new(HashMap::new()),
            resources: tokio::sync::Semaphore::new(8),
            downloads: tokio::sync::Semaphore::new(1),
        })
    }
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    pub fn set_origin(&self, origin: Url) {
        *self.origin.write().unwrap() = origin;
        self.clear();
    }
    pub fn clear(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        for (_, pending) in self.requests.lock().unwrap().drain() {
            pending.cancel.send_replace(true);
        }
    }
    fn prepare(
        &self,
        window: &WebviewWindow,
        metadata: Metadata,
        body: Vec<u8>,
    ) -> Result<reqwest::RequestBuilder> {
        let origin = self.origin.read().unwrap().clone();
        let url = endpoint(&origin, &metadata.path)?;
        let method = Method::from_bytes(metadata.method.as_bytes()).map_err(|_| UNAVAILABLE)?;
        if !matches!(
            method,
            Method::GET
                | Method::HEAD
                | Method::POST
                | Method::PUT
                | Method::PATCH
                | Method::DELETE
        ) || metadata.headers.len() > 24
        {
            return Err(UNAVAILABLE.into());
        }
        let mut request = self
            .client
            .request(method, url.clone())
            .body(body)
            .header("origin", origin.origin().ascii_serialization())
            .header("x-daat-request", "1")
            .header("x-daat-client", NATIVE_UI_MARKER);
        for (name, value) in metadata.headers {
            if !matches!(
                name.to_ascii_lowercase().as_str(),
                "accept"
                    | "content-type"
                    | "last-event-id"
                    | "idempotency-key"
                    | "x-file-name"
                    | "x-file-name-encoding"
                    | "range"
                    | "if-range"
            ) || value.len() > 16 * 1024
            {
                return Err(UNAVAILABLE.into());
            }
            request = request.header(name, value);
        }
        let cookie = read_cookies(window, &url)?;
        if !cookie.is_empty() {
            request = request.header("cookie", cookie);
        }
        Ok(request)
    }
    async fn resource(
        &self,
        window: &WebviewWindow,
        path: &str,
        method: &Method,
    ) -> Result<tauri::http::Response<Vec<u8>>> {
        let _permit = self.resources.acquire().await.map_err(|_| UNAVAILABLE)?;
        let response = self
            .prepare(
                window,
                Metadata {
                    path: path.into(),
                    method: method.to_string(),
                    headers: vec![],
                },
                vec![],
            )?
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|_| UNAVAILABLE)?;
        let mut reply = tauri::http::Response::builder().status(response.status());
        for (name, value) in response_headers(response.headers()) {
            if name != "content-security-policy" {
                reply = reply.header(name, value);
            }
        }
        reply = reply.header("content-security-policy", "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src data: blob:; connect-src 'none'; form-action 'none'; base-uri 'none'");
        let mut response = response;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| UNAVAILABLE)? {
            body.extend_from_slice(&chunk);
        }
        reply.body(body).map_err(|_| UNAVAILABLE.to_string())
    }
    fn pending(&self, id: u64) -> Result<Arc<Pending>> {
        let pending = self
            .requests
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(UNAVAILABLE)?;
        *pending.touched.lock().unwrap() = Instant::now();
        Ok(pending)
    }
    fn cancel(&self, id: u64) {
        if let Some(pending) = self.requests.lock().unwrap().remove(&id) {
            pending.cancel.send_replace(true);
        }
    }
}

#[tauri::command]
pub async fn native_http_start(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    request: Request<'_>,
) -> Result<u64> {
    authorize(&window)?;
    let metadata: Metadata = serde_json::from_str(
        request
            .headers()
            .get("x-daat-http")
            .and_then(|v| v.to_str().ok())
            .ok_or(UNAVAILABLE)?,
    )
    .map_err(|_| UNAVAILABLE)?;
    let generation = state.generation.load(Ordering::SeqCst);
    let prepared = state.prepare(&window, metadata, Vec::new())?;
    let mut requests = state.requests.lock().unwrap();
    requests.retain(|_, pending| {
        let live = pending.state.try_lock().is_err()
            || pending.touched.lock().unwrap().elapsed() < Duration::from_secs(120);
        if !live {
            pending.cancel.send_replace(true);
        }
        live
    });
    if requests.len() >= 128 {
        return Err(UNAVAILABLE.into());
    }
    let id = state.next.fetch_add(1, Ordering::Relaxed);
    let (cancel, _) = watch::channel(false);
    requests.insert(
        id,
        Arc::new(Pending {
            generation,
            cancel,
            state: AsyncMutex::new(RequestState::Ready(prepared, Vec::new())),
            touched: Mutex::new(Instant::now()),
        }),
    );
    Ok(id)
}

#[tauri::command]
pub async fn native_http_write(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    request: Request<'_>,
) -> Result<()> {
    authorize(&window)?;
    let id = request
        .headers()
        .get("x-daat-http-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(UNAVAILABLE)?;
    let chunk = match request.body() {
        InvokeBody::Raw(bytes) if bytes.len() <= MAX_CHUNK => bytes.clone(),
        InvokeBody::Json(value) if value.as_array().is_some_and(|v| v.len() <= MAX_CHUNK) => {
            serde_json::from_value::<Vec<u8>>(value.clone()).map_err(|_| UNAVAILABLE)?
        }
        _ => return Err(UNAVAILABLE.into()),
    };
    let pending = state.pending(id)?;
    let mut slot = pending.state.lock().await;
    let RequestState::Ready(_, body) = &mut *slot else {
        return Err(UNAVAILABLE.into());
    };
    if *pending.cancel.borrow() {
        state.cancel(id);
        return Err(UNAVAILABLE.into());
    }
    body.extend_from_slice(&chunk);
    Ok(())
}

#[tauri::command]
pub async fn native_http_send(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    id: u64,
) -> Result<Reply> {
    authorize(&window)?;
    let pending = state.pending(id)?;
    if pending.generation != state.generation.load(Ordering::SeqCst) {
        state.cancel(id);
        return Err(UNAVAILABLE.into());
    }
    let mut cancelled = pending.cancel.subscribe();
    let result = async {
        let mut slot = pending.state.lock().await;
        let RequestState::Ready(request, body) =
            std::mem::replace(&mut *slot, RequestState::Closed)
        else {
            return Err(UNAVAILABLE.into());
        };
        if *cancelled.borrow() {
            return Err(UNAVAILABLE.into());
        }
        let response = tokio::select! {
            _ = cancelled.changed() => return Err(UNAVAILABLE.into()),
            response = request.body(body).send() => response.map_err(|_| UNAVAILABLE)?,
        };
        write_cookies(&window, response.url(), response.headers())?;
        let reply = Reply {
            status: response.status().as_u16(),
            headers: response_headers(response.headers()),
        };
        *slot = RequestState::Reading(response, bytes::Bytes::new());
        Ok(reply)
    }
    .await;
    if result.is_err() {
        state.cancel(id);
    }
    result
}

#[tauri::command]
pub async fn native_http_read(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    id: u64,
) -> Result<tauri::ipc::Response> {
    authorize(&window)?;
    let pending = state.pending(id)?;
    let mut cancelled = pending.cancel.subscribe();
    let result: Result<Vec<u8>> = async {
        let mut slot = pending.state.lock().await;
        let RequestState::Reading(response, buffered) = &mut *slot else { return Err(UNAVAILABLE.into()); };
        loop {
            if *cancelled.borrow() { return Err(UNAVAILABLE.into()); }
            if !buffered.is_empty() {
                let count = buffered.len().min(MAX_CHUNK);
                return Ok(buffered.split_to(count).to_vec());
            }
            let chunk = tokio::select! {
                _ = cancelled.changed() => return Err(UNAVAILABLE.into()),
                chunk = tokio::time::timeout(Duration::from_secs(90), response.chunk()) => chunk.map_err(|_| UNAVAILABLE)?.map_err(|_| UNAVAILABLE)?,
            };
            match chunk {
                Some(bytes) if bytes.is_empty() => continue,
                Some(bytes) => *buffered = bytes,
                None => return Ok(Vec::new()),
            }
        }
    }.await;
    if !result.as_ref().is_ok_and(|bytes| !bytes.is_empty()) {
        state.cancel(id);
    }
    result.map(tauri::ipc::Response::new)
}

#[tauri::command]
pub async fn native_http_cancel(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    id: u64,
) -> Result<()> {
    authorize(&window)?;
    state.cancel(id);
    Ok(())
}

#[tauri::command]
pub async fn native_resource_download(
    window: WebviewWindow,
    state: tauri::State<'_, Transport>,
    path: String,
    name: String,
) -> Result<bool> {
    use std::io::Write;
    use tauri_plugin_dialog::DialogExt;
    use tauri_plugin_fs::FsExt;
    authorize(&window)?;
    let _permit = state.downloads.try_acquire().map_err(|_| UNAVAILABLE)?;
    if !resource_path(&path) {
        return Err(UNAVAILABLE.into());
    }
    let response = state.resource(&window, &path, &Method::GET).await?;
    if !response.status().is_success() {
        return Err(UNAVAILABLE.into());
    }
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '/' | '\\' | ':'))
        .take(160)
        .collect();
    let name = name.trim_matches(['.', ' ']);
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .dialog()
        .file()
        .set_file_name(if name.is_empty() { "download" } else { name })
        .save_file(move |path| {
            let _ = sender.send(path);
        });
    let Some(path) = receiver.await.map_err(|_| UNAVAILABLE)? else {
        return Ok(false);
    };
    let app = window.app_handle().clone();
    tauri::async_runtime::spawn_blocking(move || {
        let mut options = tauri_plugin_fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        let mut file = app.fs().open(path, options).map_err(|_| UNAVAILABLE)?;
        file.write_all(response.body()).map_err(|_| UNAVAILABLE)?;
        file.flush().map_err(|_| UNAVAILABLE)?;
        Ok(true)
    })
    .await
    .map_err(|_| UNAVAILABLE)?
}

pub fn capability(app: &tauri::AppHandle) -> tauri::Result<()> {
    app.add_capability(serde_json::json!({
        "identifier":"native-client", "windows":["main"], "local":true,
        "permissions":["allow-desktop-account", "allow-desktop-window", "allow-phone-use-status", "allow-phone-use-configure", "allow-phone-use-display-configure", "allow-computer-use-status", "allow-computer-use-configure", "allow-computer-use-display-configure", "allow-computer-use-permission", "allow-native-http-start", "allow-native-http-write", "allow-native-http-send", "allow-native-http-read", "allow-native-http-cancel", "allow-native-resource-download"]
    }).to_string())
}

/// Local device capability settings still require the profile's current Owner session.
#[cfg(any(target_os = "android", target_os = "macos"))]
pub async fn authorize_owner(window: &WebviewWindow) -> Result<()> {
    authorize(window)?;
    let state = window.state::<Transport>();
    let response = state
        .prepare(
            window,
            Metadata {
                path: "/v1/stage".into(),
                method: "GET".into(),
                headers: vec![],
            },
            vec![],
        )?
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| UNAVAILABLE)?;
    if !response.status().is_success() {
        return Err("Owner login required".into());
    }
    Ok(())
}

pub fn resources(builder: tauri::Builder<tauri::Wry>) -> tauri::Builder<tauri::Wry> {
    builder
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_dialog::init())
        .register_asynchronous_uri_scheme_protocol("dl-resource", |context, request, responder| {
            let app = context.app_handle().clone();
            let main = context.webview_label() == "main";
            tauri::async_runtime::spawn(async move {
                let result = async {
                    if !main
                        || !matches!(*request.method(), Method::GET | Method::HEAD)
                        || !Url::parse(&request.uri().to_string())
                            .is_ok_and(|url| resource_url(&url))
                    {
                        return Err(UNAVAILABLE.into());
                    }
                    let path = request.uri().path();
                    if !resource_path(path) || request.uri().query().is_some() {
                        return Err(UNAVAILABLE.into());
                    }
                    let window = app.get_webview_window("main").ok_or(UNAVAILABLE)?;
                    authorize(&window)?;
                    app.state::<Transport>()
                        .resource(&window, path, request.method())
                        .await
                }
                .await;
                responder.respond(result.unwrap_or_else(|_: String| {
                    tauri::http::Response::builder()
                        .status(403)
                        .body(Vec::new())
                        .unwrap()
                }));
            });
        })
}
fn resource_path(path: &str) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    matches!(parts.as_slice(), ["", "v1", "actors", actor, "resources", resource] if !actor.is_empty() && !resource.is_empty())
        || matches!(parts.as_slice(), ["", "v1", "stages", stage, "actors", actor, "resources", resource] if !stage.is_empty() && !actor.is_empty() && !resource.is_empty())
}

#[cfg(windows)]
fn read_cookies(window: &WebviewWindow, url: &Url) -> Result<String> {
    Ok(window
        .cookies_for_url(url.clone())
        .map_err(|_| UNAVAILABLE)?
        .into_iter()
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; "))
}
#[cfg(target_os = "macos")]
fn read_cookies(window: &WebviewWindow, url: &Url) -> Result<String> {
    // Wry's WKWebView cookies_for_url compares Cookie.domain with Url::domain;
    // the latter is None for IP hosts. Keep loopback cookies in WebKit's private
    // store and filter with the exact, already verified service host instead.
    Ok(window
        .cookies()
        .map_err(|_| UNAVAILABLE)?
        .into_iter()
        .filter(|cookie| cookie_matches_service(cookie, url))
        .map(|cookie| format!("{}={}", cookie.name(), cookie.value()))
        .collect::<Vec<_>>()
        .join("; "))
}
#[cfg(target_os = "macos")]
fn cookie_matches_service(cookie: &cookie::Cookie<'_>, url: &Url) -> bool {
    let path = cookie.path().unwrap_or("/");
    cookie.domain() == url.host_str()
        && (url.path() == path
            || url
                .path()
                .strip_prefix(path)
                .is_some_and(|suffix| path.ends_with('/') || suffix.starts_with('/')))
        && cookie
            .expires_datetime()
            .is_none_or(|expires| expires > cookie::time::OffsetDateTime::now_utc())
        && (!cookie.secure().unwrap_or(false)
            || url.scheme() == "https"
            || matches!(url.host(),Some(url::Host::Ipv4(ip)) if ip.is_loopback())
            || matches!(url.host(),Some(url::Host::Ipv6(ip)) if ip.is_loopback()))
}
#[cfg(not(target_os = "android"))]
fn write_cookies(window: &WebviewWindow, url: &Url, headers: &HeaderMap) -> Result<()> {
    for value in headers.get_all("set-cookie") {
        let mut cookie = cookie::Cookie::parse(value.to_str().map_err(|_| UNAVAILABLE)?.to_owned())
            .map_err(|_| UNAVAILABLE)?;
        cookie.set_domain(url.host_str().ok_or(UNAVAILABLE)?.to_owned());
        if let Some(age) = cookie.max_age() {
            cookie.set_expires(cookie::time::OffsetDateTime::now_utc() + age);
        }
        window.set_cookie(cookie).map_err(|_| UNAVAILABLE)?;
    }
    Ok(())
}
#[cfg(target_os = "android")]
fn read_cookies(_window: &WebviewWindow, url: &Url) -> Result<String> {
    android_cookies(url, None)
}
#[cfg(target_os = "android")]
fn write_cookies(_window: &WebviewWindow, url: &Url, headers: &HeaderMap) -> Result<()> {
    for value in headers.get_all("set-cookie") {
        android_cookies(url, Some(value.to_str().map_err(|_| UNAVAILABLE)?))?;
    }
    Ok(())
}
#[cfg(target_os = "android")]
fn android_cookies(url: &Url, value: Option<&str>) -> Result<String> {
    use jni::{jni_sig, jni_str, objects::JString};
    let context = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(context.vm().cast()) };
    vm.attach_current_thread(|env| -> jni::errors::Result<String> {
        let manager = env
            .call_static_method(
                jni_str!("android/webkit/CookieManager"),
                jni_str!("getInstance"),
                jni_sig!("()Landroid/webkit/CookieManager;"),
                &[],
            )?
            .l()?;
        let url = env.new_string(url.as_str())?;
        if let Some(value) = value {
            let value = env.new_string(value)?;
            env.call_method(
                &manager,
                jni_str!("setCookie"),
                jni_sig!("(Ljava/lang/String;Ljava/lang/String;)V"),
                &[(&url).into(), (&value).into()],
            )?;
            env.call_method(&manager, jni_str!("flush"), jni_sig!("()V"), &[])?;
            Ok(String::new())
        } else {
            let value = env
                .call_method(
                    &manager,
                    jni_str!("getCookie"),
                    jni_sig!("(Ljava/lang/String;)Ljava/lang/String;"),
                    &[(&url).into()],
                )?
                .l()?;
            if value.is_null() {
                return Ok(String::new());
            }
            env.cast_local::<JString>(value)?.try_to_string(env)
        }
    })
    .map_err(|_| UNAVAILABLE.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_requests_and_resources_cannot_escape_the_verified_endpoint() {
        let origin = Url::parse("http://127.0.0.1:45123").unwrap();
        let valid = "/v1/actors/core/messages/search?q=%E4%BD%A0%2F..";
        assert_eq!(
            endpoint(&origin, valid).unwrap().as_str(),
            format!("{origin}{}", &valid[1..])
        );
        for path in [
            "https://example.com/v1/access",
            "//example.com/v1/access",
            "/v1/../private",
            "/v1/%2e%2e/private",
            "/v1/actors%2fcore",
            "/v1/\\evil",
            "/v1/access#fragment",
        ] {
            assert!(endpoint(&origin, path).is_err(), "{path}");
        }
        assert!(resource_path("/v1/actors/core/resources/image"));
        assert!(!resource_path("/v1/account/session"));
        assert!(!resource_path("/v1/actors/core/resources/image/extra"));
        for url in [
            "https://tauri.localhost.evil/",
            "http://tauri.localhost:45123/",
            "dl-resource://localhost/",
            "file:///index.html",
            "tauri://user@localhost/",
        ] {
            assert!(!app_url(&Url::parse(url).unwrap()));
        }
    }

    #[test]
    fn native_response_metadata_never_exposes_session_cookies_or_authorization() {
        let mut headers = HeaderMap::new();
        headers.insert("set-cookie", "secret=private; HttpOnly".parse().unwrap());
        headers.insert("authorization", "private".parse().unwrap());
        headers.insert("retry-after", "2".parse().unwrap());
        headers.insert("content-type", "text/event-stream".parse().unwrap());
        let visible = response_headers(&headers);
        assert_eq!(visible.len(), 2);
        assert!(visible.contains(&("retry-after".into(), "2".into())));
        assert!(visible.contains(&("content-type".into(), "text/event-stream".into())));
    }
}

#[cfg(all(test, target_os = "macos"))]
mod macos_cookie_tests {
    use super::*;
    #[test]
    fn profile_cookie_matches_loopback_ip_without_covering_other_hosts_or_paths() {
        let cookie = cookie::Cookie::build(("session", "fixture"))
            .domain("127.0.0.1")
            .path("/v1")
            .http_only(true)
            .build();
        assert!(cookie_matches_service(
            &cookie,
            &Url::parse("http://127.0.0.1:45825/v1/access").unwrap()
        ));
        for url in [
            "http://127.0.0.2/v1/access",
            "http://localhost/v1/access",
            "http://127.0.0.1/v10/access",
            "http://example.com/v1/access",
        ] {
            assert!(!cookie_matches_service(&cookie, &Url::parse(url).unwrap()));
        }
        let expired = cookie::Cookie::build(("session", "fixture"))
            .domain("127.0.0.1")
            .path("/")
            .expires(cookie::time::OffsetDateTime::UNIX_EPOCH)
            .build();
        assert!(!cookie_matches_service(
            &expired,
            &Url::parse("http://127.0.0.1/v1/access").unwrap()
        ));
    }
}
