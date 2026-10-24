//! A transparent shell above independent WKWebViews in the native view tree.
//! Hit regions affect input only; Core Animation composes every CSS pixel.
use daat_locus_client_protocol::web::{BrowserBounds, BrowserHitRegion};
use objc2::{DefinedClass, MainThreadOnly, define_class, msg_send, rc::Retained};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};
use objc2_web_kit::WKWebView;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
};
use tauri::Webview;

#[derive(Default)]
struct Ivars {
    viewport: Cell<Option<NSRect>>,
    foreground: RefCell<Vec<NSRect>>,
    flipped: Cell<bool>,
}
fn contains(rect: NSRect, p: NSPoint) -> bool {
    p.x >= rect.origin.x
        && p.y >= rect.origin.y
        && p.x < rect.origin.x + rect.size.width
        && p.y < rect.origin.y + rect.size.height
}
define_class!(
    #[unsafe(super=NSView)]
    #[thread_kind=MainThreadOnly]
    #[ivars=Ivars]
    struct ShellHost;
    unsafe impl NSObjectProtocol for ShellHost {}
    impl ShellHost {
        #[unsafe(method(isFlipped))]
        fn flipped(&self)->bool {self.ivars().flipped.get()}
        #[unsafe(method_id(hitTest:))]
        fn hit(&self,point:NSPoint)->Option<Retained<NSView>> {
            let local=self.convertPoint_fromView(point,unsafe{self.superview()}.as_deref());
            if self.ivars().viewport.get().is_some_and(|area|contains(area,local))
                && !self.ivars().foreground.borrow().iter().any(|area|contains(*area,local)) {
                None
            }else{
                let hit:Option<Retained<NSView>>=unsafe{msg_send![super(self),hitTest:point]};
                hit.filter(|view|Retained::as_ptr(view).cast::<c_void>()!=(self as *const Self).cast::<c_void>())
            }
        }
    }
);
thread_local! {static HOSTS:RefCell<HashMap<String,Retained<ShellHost>>>=RefCell::new(HashMap::new());}
pub(crate) fn remove(label: &str) {
    HOSTS.with(|hosts| {
        if let Some(host) = hosts.borrow_mut().remove(label) {
            unsafe {
                host.removeFromSuperview();
            }
        }
    });
}
pub(crate) async fn compose(
    shell: &Webview,
    bounds: Option<BrowserBounds>,
    foreground: Vec<BrowserHitRegion>,
) -> Result<(), String> {
    let label = shell.label().to_owned();
    let (tx, rx) = tokio::sync::oneshot::channel();
    shell.with_webview(move|platform|{
        let result=(||unsafe {
            let web=&*platform.inner().cast::<WKWebView>();
            let mtm=MainThreadMarker::new().ok_or("Browser composition requires the main thread")?;
            let wrapper=if let Some(host)=HOSTS.with(|hosts|hosts.borrow().get(&label).cloned()){host}else{
                let parent=web.superview().ok_or("Shell parent unavailable")?;
                let allocated=ShellHost::alloc(mtm).set_ivars(Ivars{flipped:Cell::new(parent.isFlipped()),..Default::default()});
                let host:Retained<ShellHost>=msg_send![super(allocated),initWithFrame:parent.bounds()];
                host.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable|NSAutoresizingMaskOptions::ViewHeightSizable);
                let frame=web.frame();
                parent.addSubview(&host);host.addSubview(web);web.setFrame(frame);
                HOSTS.with(|hosts|{hosts.borrow_mut().insert(label,host.clone());});host
            };
            if let Some(parent)=wrapper.superview(){
                // AppKit and Core Animation share this order. Never mask the page.
                if parent.subviews().lastObject().is_none_or(|top|Retained::as_ptr(&top).cast::<c_void>()!=Retained::as_ptr(&wrapper).cast::<c_void>()) {
                    let _:()=msg_send![&*parent,addSubview:&*wrapper positioned:1_isize relativeTo:None::<&NSView>];
                }
                wrapper.setFrame(parent.bounds());
            }
            let area=wrapper.bounds();
            let factor=bounds.map(|b|b.scale/web.window().map(|w|w.backingScaleFactor()).unwrap_or(1.0)).unwrap_or(1.0);
            let rect=|x:f64,y:f64,w:f64,h:f64|NSRect::new(NSPoint::new(x*factor,if wrapper.isFlipped(){y*factor}else{area.size.height-(y+h)*factor}),NSSize::new(w*factor,h*factor));
            wrapper.ivars().viewport.set(bounds.map(|b|rect(b.x,b.y,b.width,b.height)));
            *wrapper.ivars().foreground.borrow_mut()=foreground.into_iter().map(|r|rect(r.x,r.y,r.width,r.height)).collect();
            Ok::<_,&str>(())
        })().map_err(str::to_owned);
        let _=tx.send(result);
    }).map_err(|e|e.to_string())?;
    rx.await.map_err(|e| e.to_string())?
}
