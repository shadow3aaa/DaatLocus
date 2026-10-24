//! Isolate WebKit key redispatch and preserve page-controlled editing defaults.
use daat_locus_client_protocol::web::BrowserInput;
use objc2::{
    ffi, msg_send,
    rc::Retained,
    runtime::{AnyObject, Imp, Sel},
    sel,
};
use objc2_app_kit::NSEvent;
use objc2_foundation::{NSObjectProtocol, NSString};
use objc2_web_kit::WKWebView;
use std::{
    ffi::c_void,
    sync::{Mutex, OnceLock},
    time::Duration,
};
use tauri::Webview;

static TARGET: u8 = 0;
type SendEvent = unsafe extern "C-unwind" fn(*mut AnyObject, Sel, *mut NSEvent);
static ORIGINAL: OnceLock<SendEvent> = OnceLock::new();
pub(crate) fn synthetic(event: &NSEvent) -> bool {
    unsafe {
        !ffi::objc_getAssociatedObject(
            (event as *const NSEvent).cast(),
            (&TARGET as *const u8).cast::<c_void>(),
        )
        .is_null()
    }
}
unsafe extern "C-unwind" fn send_event(app: *mut AnyObject, selector: Sel, event: *mut NSEvent) {
    unsafe {
        // Direct NSApplication.sendEvent redispatch bypasses local monitors.
        // An unhandled model key must never reach another view or client menu.
        if !event.as_ref().is_some_and(synthetic) {
            ORIGINAL.get().unwrap()(app, selector, event);
        }
    }
}
pub(crate) fn tag(event: &NSEvent, web: &WKWebView) -> Result<(), String> {
    unsafe {
        // This process's concrete application class; physical Owner events use
        // its original implementation. No OS event is posted or intercepted.
        if ORIGINAL.get().is_none() {
            let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
            let method = (*app)
                .class()
                .instance_method(sel!(sendEvent:))
                .ok_or("Native browser key isolation unavailable")?;
            let original = std::mem::transmute::<Imp, SendEvent>(method.implementation());
            ORIGINAL
                .set(original)
                .map_err(|_| "Native browser key isolation already initialized")?;
            method.set_implementation(std::mem::transmute::<SendEvent, Imp>(send_event));
        }
        ffi::objc_setAssociatedObject(
            (event as *const NSEvent).cast_mut().cast(),
            (&TARGET as *const u8).cast::<c_void>(),
            (web as *const WKWebView).cast_mut().cast(),
            ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
        );
        Ok(())
    }
}

pub(crate) struct Editing {
    command: &'static str,
    marker: &'static str,
    key: String,
    shift: bool,
    meta: bool,
    control: bool,
    token: String,
}
pub(crate) fn editing(input: &BrowserInput) -> Option<Editing> {
    let BrowserInput::Key { key, modifiers } = input else {
        return None;
    };
    let meta = modifiers.iter().any(|m| m == "meta");
    let control = modifiers.iter().any(|m| m == "control");
    let shift = modifiers.iter().any(|m| m == "shift");
    let key = key.to_ascii_lowercase();
    if modifiers.iter().any(|m| m == "alt")
        || meta && control
        || !meta && !(control && key == "a" && !shift)
    {
        return None;
    }
    let (command, marker) = match key.as_str() {
        "a" if !shift => ("SelectAll", "selection"),
        "c" if !shift => ("Copy", "copy"),
        "x" if !shift => ("Cut", "cut"),
        "v" => (if shift { "PasteAsPlainText" } else { "Paste" }, "paste"),
        _ => return None,
    };
    Some(Editing {
        command,
        marker,
        key,
        shift,
        meta,
        control,
        token: uuid::Uuid::new_v4().simple().to_string(),
    })
}
fn script(action: &str, editing: &Editing) -> String {
    format!(
        "({})({})",
        include_str!("browser_keys_macos.js"),
        serde_json::json!({"action":action,"key":editing.key,"shift":editing.shift,"meta":editing.meta,"control":editing.control,"marker":editing.marker,"token":editing.token})
    )
}
pub(crate) async fn begin(view: &Webview, editing: &Editing) -> Result<(), String> {
    inspect(view, script("begin", editing)).await?;
    Ok(())
}
pub(crate) async fn finish(view: &Webview, editing: Editing) -> Result<(), String> {
    let state = inspect(view, script("finish", &editing)).await?;
    if state["delivered"] != true {
        return Err("Browser key delivery was interrupted; observe before continuing".into());
    }
    if state["prevented"] == true || state["defaulted"] == true {
        return Ok(());
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move |platform|unsafe {
        let web=&*platform.inner().cast::<WKWebView>();
        if super::page_responder(web).is_err(){let _=tx.send(Err("Browser focus changed during key input".to_owned()));return;}
        if !web.respondsToSelector(sel!(_executeEditCommand:argument:completion:)){let _=tx.send(Err("Native editing unavailable".to_owned()));return;}
        let tx=Mutex::new(Some(tx));
        let done=block2::RcBlock::new(move |_:objc2::runtime::Bool|{if let Some(tx)=tx.lock().unwrap().take(){let _=tx.send(Ok(()));}});
        let _:()=msg_send![web,_executeEditCommand:&*NSString::from_str(editing.command),argument:&*NSString::from_str(""),completion:&*done];
    }).map_err(|_|"Native editing unavailable")?;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| "Native editing timed out; do not replay")?
        .map_err(|_| "Native editing interrupted")?
}

// Resolve the native focused frame, including opaque/cross-origin iframes, and
// inspect key completion in WebKit's isolated client world. Page scripts cannot
// forge this state. Neither the frame data nor an evaluation API reaches models.
async fn inspect(view: &Webview, script: String) -> Result<serde_json::Value, String> {
    use std::sync::Arc;
    type Pending =
        Arc<Mutex<Option<tokio::sync::oneshot::Sender<Result<serde_json::Value, String>>>>>;
    fn reply(pending: &Pending, value: Result<serde_json::Value, String>) {
        if let Some(sender) = pending.lock().unwrap().take() {
            let _ = sender.send(value);
        }
    }
    let (tx, rx) = tokio::sync::oneshot::channel();
    let pending = Arc::new(Mutex::new(Some(tx)));
    view.with_webview(move |platform| unsafe {
        let web = Retained::retain(platform.inner().cast::<WKWebView>()).unwrap();
        if !web.respondsToSelector(sel!(_frames:)) || !web.respondsToSelector(sel!(callAsyncJavaScript:arguments:inFrame:inContentWorld:completionHandler:)) {
            reply(&pending, Err("Native focused-frame keyboard inspection unavailable".into()));return;
        }
        let body = NSString::from_str(&format!("return JSON.stringify(await ({script}));"));
        let inspected_web = web.clone();
        let frames = block2::RcBlock::new(move |root: *mut AnyObject| {
            let result = (|| -> Result<(), String> {
                let root = Retained::retain(root).ok_or("Browser frame tree unavailable")?;
                let supported:bool=msg_send![&*root,respondsToSelector:sel!(info)];
                if !supported { return Err("Native Command editing requires macOS 14.4 or newer".into()); }
                let root_info: Retained<AnyObject> = msg_send![&*root, info];
                let mut focused = None;
                let mut queue = vec![root];let mut visited = 0;
                while let Some(node) = queue.pop() {
                    visited += 1;
                    let info: Retained<AnyObject> = msg_send![&*node, info];
                    let supported:bool=msg_send![&*info,respondsToSelector:sel!(_isFocused)];
                    if !supported { return Err("Native Command editing requires macOS 14.4 or newer".into()); }
                    let active: bool = msg_send![&*info, _isFocused];
                    if active {
                        if focused.is_some() {return Err("Browser focus changed during keyboard inspection".into());}
                        focused = Some(info);
                    }
                    let children: Retained<AnyObject> = msg_send![&*node, childFrames];
                    let count: usize = msg_send![&*children, count];
                    if visited + queue.len() + count > 256 { return Err("Browser frame tree exceeds keyboard inspection limit".into()); }
                    for index in 0..count {let child:Retained<AnyObject>=msg_send![&*children,objectAtIndex:index];queue.push(child);}
                }
                let focused = focused.unwrap_or(root_info);
                let world: Retained<AnyObject> = msg_send![objc2::class!(WKContentWorld), defaultClientWorld];
                let complete = pending.clone();
                let done = block2::RcBlock::new(move |result: *mut AnyObject, error: *mut AnyObject| {
                    let result = (|| {
                        if !error.is_null() || result.is_null() { return Err("Native key inspection failed; do not replay".into()); }
                        let string:bool=msg_send![result,isKindOfClass:objc2::class!(NSString)];
                        if !string {return Err("Invalid native key inspection".into());}
                        let text = (&*result.cast::<NSString>()).to_string();
                        if text.len() > 16384 {return Err("Native key inspection exceeds its limit".into());}
                        serde_json::from_str(&text).map_err(|_| "Invalid native key inspection".into())
                    })();
                    reply(&complete, result);
                });
                let arguments=objc2_foundation::NSDictionary::<NSString,AnyObject>::new();
                let _:()=msg_send![&*inspected_web,callAsyncJavaScript:&*body,arguments:&*arguments,inFrame:&*focused,inContentWorld:&*world,completionHandler:&*done];
                Ok(())
            })();
            if let Err(error)=result {reply(&pending,Err(error));}
        });
        let _: () = msg_send![&*web, _frames:&*frames];
    }).map_err(|_| "Native keyboard inspection unavailable")?;
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .map_err(|_| "Native key inspection timed out; do not replay")?
        .map_err(|_| "Native key inspection interrupted")?
}
