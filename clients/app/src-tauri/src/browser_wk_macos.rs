//! WebKit delegates owned by isolated browser pages, never by the account view.
use objc2::{
    DefinedClass, MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Sel},
    sel,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::WKWebView;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::HashMap, time::Duration};
use tauri::Webview;

type Pending = (String, tokio::sync::oneshot::Sender<Result<Value, String>>);
#[derive(Default)]
struct FindState {
    pending: RefCell<Option<Pending>>,
}
define_class!(
    #[unsafe(super=NSObject)]
    #[thread_kind=MainThreadOnly]
    #[ivars=FindState]
    struct FindDelegate;
    unsafe impl NSObjectProtocol for FindDelegate {}
    impl FindDelegate {
        #[unsafe(method(_webView:didFindMatches:forString:withMatchIndex:))]
        fn matched(&self, _web: &WKWebView, count: usize, text: &NSString, index: isize) {
            self.complete(text, json!({"found":count>0,"count":count,"index":if count>0 {index+1} else {0}}));
        }
        #[unsafe(method(_webView:didFailToFindString:))]
        fn missing(&self, _web: &WKWebView, text: &NSString) {
            self.complete(text, json!({"found":false,"count":0,"index":0}));
        }
    }
);
impl FindDelegate {
    fn complete(&self, text: &NSString, result: Value) {
        let mut pending = self.ivars().pending.borrow_mut();
        if pending
            .as_ref()
            .is_some_and(|(query, _)| query == &text.to_string())
            && let Some((_, sender)) = pending.take()
        {
            let _ = sender.send(Ok(result));
        }
    }
}

struct NavigationState {
    original: Retained<AnyObject>,
    shared: bool,
}
define_class!(
    #[unsafe(super=NSObject)]
    #[thread_kind=MainThreadOnly]
    #[ivars=NavigationState]
    struct BrowserNavigation;
    unsafe impl NSObjectProtocol for BrowserNavigation {}
    impl BrowserNavigation {
        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]
        fn navigation(&self, web: &WKWebView, action: &AnyObject, handler: &block2::Block<dyn Fn(isize)>) {
            unsafe {
                let frame:Option<Retained<AnyObject>>=msg_send![action,targetFrame];
                let subframe=frame.is_some_and(|frame|{let main:bool=msg_send![&*frame,isMainFrame];!main});
                let request:Retained<AnyObject>=msg_send![action,request];
                let url:Option<Retained<AnyObject>>=msg_send![&*request,URL];
                let absolute:Option<Retained<NSString>>=url.and_then(|url|msg_send![&*url,absoluteString]);
                if subframe && absolute.is_some_and(|url|matches!(url.to_string().as_str(),"about:srcdoc"|"about:blank")) {
                    // These documents inherit their embedding page's sandbox.
                    // Retain Wry/host policy for every top-level navigation.
                    handler.call((1,));
                } else {
                    let _:()=msg_send![&*self.ivars().original,webView:web,decidePolicyForNavigationAction:action,decisionHandler:handler];
                }
            }
        }
        #[unsafe(method(respondsToSelector:))]
        fn responds(&self, selector: Sel) -> bool {
            unsafe { let own:bool=msg_send![super(self), respondsToSelector:selector];
                own || {let original:bool=msg_send![&*self.ivars().original, respondsToSelector:selector]; original} }
        }
        #[unsafe(method_id(forwardingTargetForSelector:))]
        fn forward(&self, _selector: Sel) -> Option<Retained<AnyObject>> { Some(self.ivars().original.clone()) }
        #[unsafe(method(webView:decidePolicyForNavigationResponse:decisionHandler:))]
        fn response(&self, web: &WKWebView, response: &AnyObject, handler: &block2::Block<dyn Fn(isize)>) {
            unsafe {
                let native:Retained<AnyObject>=msg_send![response,response];
                let has_headers:bool=msg_send![&*native,respondsToSelector:sel!(valueForHTTPHeaderField:)];
                let disposition:Option<Retained<NSString>>=if has_headers {msg_send![&*native,valueForHTTPHeaderField:&*NSString::from_str("Content-Disposition")]}else{None};
                if self.ivars().shared && disposition.is_some_and(|value|value.to_string().split(';').next().unwrap_or_default().trim().eq_ignore_ascii_case("attachment")) {
                    // WKNavigationResponsePolicyDownload. Wry currently checks
                    // only canShowMIMEType, losing renderable attachments.
                    handler.call((2,));
                } else {
                    let _:()=msg_send![&*self.ivars().original,webView:web,decidePolicyForNavigationResponse:response,decisionHandler:handler];
                }
            }
        }
    }
);
thread_local! {
    static FIND: RefCell<HashMap<String,Retained<FindDelegate>>> = RefCell::new(HashMap::new());
    static NAVIGATION: RefCell<HashMap<String,Retained<BrowserNavigation>>> = RefCell::new(HashMap::new());
}
pub(crate) fn remove(label: &str) {
    FIND.with(|delegates| {
        delegates.borrow_mut().remove(label);
    });
    NAVIGATION.with(|delegates| {
        delegates.borrow_mut().remove(label);
    });
}
pub(crate) fn prepare(view: &Webview, shared: bool) {
    let label = view.label().to_owned();
    let _ = view.with_webview(move |platform| unsafe {
        let web = &*platform.inner().cast::<WKWebView>();
        // The shared shell already reserves titlebar/toolbar space. WebKit's
        // automatic titlebar inset shifts trusted input away from screenshots,
        // including when the backstage window is hidden.
        if web.respondsToSelector(sel!(_setAutomaticallyAdjustsContentInsets:)) {
            let _: () = msg_send![web,_setAutomaticallyAdjustsContentInsets:false];
            let _: () = msg_send![web,_setTopContentInset:0.0f64];
        }
        let original: Option<Retained<AnyObject>> = msg_send![web, navigationDelegate];
        if let Some(original) = original
            && let Some(main) = MainThreadMarker::new()
        {
            let allocated =
                BrowserNavigation::alloc(main).set_ivars(NavigationState { original, shared });
            let delegate: Retained<BrowserNavigation> = msg_send![super(allocated), init];
            let _: () = msg_send![web,setNavigationDelegate:&*delegate];
            NAVIGATION.with(|delegates| {
                delegates.borrow_mut().insert(label, delegate);
            });
        }
    });
}

pub(crate) async fn find(view: &Webview, action: &str, text: &str) -> Result<Value, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let label = view.label().to_owned();
    let action = action.to_owned();
    let text = text.to_owned();
    view.with_webview(move |platform|unsafe {
        let web=&*platform.inner().cast::<WKWebView>();
        if !web.respondsToSelector(sel!(_findString:options:maxCount:)) || !web.respondsToSelector(sel!(_setFindDelegate:)) || !web.respondsToSelector(sel!(_hideFindUI)) {
            let _=sender.send(Err("Native page search unavailable".into()));return;
        }
        FIND.with(|delegates| {
            let mut delegates=delegates.borrow_mut();
            let delegate=delegates.entry(label).or_insert_with(|| {
                let allocated=FindDelegate::alloc(MainThreadMarker::new().unwrap()).set_ivars(FindState::default());
                let delegate:Retained<FindDelegate>=msg_send![super(allocated),init];
                let _:()=msg_send![web,_setFindDelegate:&*delegate];delegate
            });
            if let Some((_,previous))=delegate.ivars().pending.borrow_mut().take() {let _=previous.send(Err("Page search superseded".into()));}
            if action=="find_close" || text.is_empty() {
                let _:()=msg_send![web,_hideFindUI];
                let _=sender.send(Ok(json!({"found":false,"count":0,"index":0})));
            } else {
                if action=="find" {let _:()=msg_send![web,_hideFindUI];}
                *delegate.ivars().pending.borrow_mut()=Some((text.clone(),sender));
                // _WKFindOptions: case insensitive, wrap, native indicator,
                // highlight all matches and report the active match index.
                let options:usize=1|16|64|128|512|if action=="find_previous" {8}else{0};
                let _:()=msg_send![web,_findString:&*NSString::from_str(&text),options:options,maxCount:10000usize];
            }
        });
    }).map_err(|_|"Native page search unavailable")?;
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .map_err(|_| "Page search timed out")?
        .map_err(|_| "Page search interrupted")?
}
