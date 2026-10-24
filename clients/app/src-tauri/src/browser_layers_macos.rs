//! Native composition and hit testing only; no script is installed in the page.
use daat_locus_client_protocol::web::{BrowserBounds, BrowserOcclusion};
use objc2::{DefinedClass, MainThreadOnly, class, define_class, msg_send};
use objc2::{rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSAutoresizingMaskOptions, NSView};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSString};
use objc2_web_kit::WKWebView;
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::c_void,
};
use tauri::Webview;

#[derive(Clone)]
struct Cut {
    rect: NSRect,
    radius: f64,
}
impl Cut {
    fn contains(&self, point: NSPoint) -> bool {
        let r = self.radius;
        let x = self.rect.origin.x;
        let y = self.rect.origin.y;
        let right = x + self.rect.size.width;
        let bottom = y + self.rect.size.height;
        if point.x < x || point.y < y || point.x >= right || point.y >= bottom {
            return false;
        }
        let cx = point.x.clamp(x + r, right - r);
        let cy = point.y.clamp(y + r, bottom - r);
        (point.x - cx).powi(2) + (point.y - cy).powi(2) <= r * r
    }
}
#[derive(Default)]
struct Ivars {
    cuts: RefCell<Vec<Cut>>,
    flipped: Cell<bool>,
}
define_class!(
    #[unsafe(super=NSView)]
    #[thread_kind=MainThreadOnly]
    #[ivars=Ivars]
    struct ClipHost;
    unsafe impl NSObjectProtocol for ClipHost {}
    impl ClipHost{
        #[unsafe(method(isFlipped))]
        fn flipped(&self)->bool{self.ivars().flipped.get()}
        #[unsafe(method_id(hitTest:))]
        fn hit(&self,point:NSPoint)->Option<Retained<NSView>>{
            let local=self.convertPoint_fromView(point,self.superview().as_deref());
            if self.ivars().cuts.borrow().iter().any(|cut|cut.contains(local)){return None;}
            let hit:Option<Retained<NSView>>=unsafe{msg_send![super(self),hitTest:point]};
            hit.filter(|view|Retained::as_ptr(view).cast::<c_void>()!=(self as *const Self).cast::<c_void>())
        }
    }
);
thread_local! {static HOSTS:RefCell<HashMap<String,Retained<ClipHost>>>=RefCell::new(HashMap::new());}
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPathCreateMutable() -> *mut c_void;
    fn CGPathAddRect(path: *mut c_void, transform: *const c_void, rect: NSRect);
    fn CGPathAddRoundedRect(
        path: *mut c_void,
        transform: *const c_void,
        rect: NSRect,
        rx: f64,
        ry: f64,
    );
    fn CGPathRelease(path: *mut c_void);
}
#[link(name = "QuartzCore", kind = "framework")]
unsafe extern "C" {}
pub(crate) fn remove(label: &str) {
    HOSTS.with(|hosts| {
        if let Some(host) = hosts.borrow_mut().remove(label) {
            host.removeFromSuperview();
        }
    });
}
pub(crate) async fn occlude(
    view: &Webview,
    bounds: Option<BrowserBounds>,
    cuts: Vec<BrowserOcclusion>,
) -> Result<(), String> {
    let label = view.label().to_owned();
    let main = view.window().label() == "main";
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move |platform| {
        let result = (|| unsafe {
            let web = &*platform.inner().cast::<WKWebView>();
            if !main {
                remove(&label);
                return Ok(());
            }
            let mtm =
                MainThreadMarker::new().ok_or("Browser composition requires the main thread")?;
            let parent = web.superview().ok_or("Browser parent unavailable")?;
            let existing = HOSTS.with(|hosts| hosts.borrow().get(&label).cloned());
            let wrapper = if let Some(host) = existing.filter(|host| {
                Retained::as_ptr(host).cast::<c_void>()
                    == Retained::as_ptr(&parent).cast::<c_void>()
            }) {
                host
            } else {
                remove(&label);
                let frame = web.frame();
                let allocated = ClipHost::alloc(mtm).set_ivars(Ivars {
                    cuts: Default::default(),
                    flipped: Cell::new(parent.isFlipped()),
                });
                let host: Retained<ClipHost> =
                    msg_send![super(allocated),initWithFrame:parent.bounds()];
                host.setAutoresizingMask(
                    NSAutoresizingMaskOptions::ViewWidthSizable
                        | NSAutoresizingMaskOptions::ViewHeightSizable,
                );
                parent.addSubview(&host);
                host.addSubview(web);
                web.setFrame(frame);
                HOSTS.with(|hosts| {
                    hosts.borrow_mut().insert(label, host.clone());
                });
                host
            };
            if let Some(parent) = wrapper.superview() {
                wrapper.setFrame(parent.bounds());
            }
            let area = wrapper.bounds();
            let factor = bounds
                .map(|b| b.scale / web.window().map(|w| w.backingScaleFactor()).unwrap_or(1.0))
                .unwrap_or(1.0);
            let native: Vec<_> = if bounds.is_none() {
                Vec::new()
            } else {
                cuts.into_iter()
                    .map(|cut| {
                        let width = cut.width * factor;
                        let height = cut.height * factor;
                        let x = cut.x * factor;
                        let y = if wrapper.isFlipped() {
                            cut.y * factor
                        } else {
                            area.size.height - (cut.y + cut.height) * factor
                        };
                        Cut {
                            rect: NSRect::new(
                                NSPoint::new(x, y),
                                objc2_foundation::NSSize::new(width, height),
                            ),
                            radius: (cut.radius * factor).min(width.min(height) / 2.0),
                        }
                    })
                    .collect()
            };
            *wrapper.ivars().cuts.borrow_mut() = native.clone();
            wrapper.setWantsLayer(true);
            let layer: *mut AnyObject = msg_send![&*wrapper, layer];
            if layer.is_null() {
                return Err("Browser composition unavailable");
            }
            let mut mask: Option<Retained<AnyObject>> = None;
            for cut in native {
                let path = CGPathCreateMutable();
                if path.is_null() {
                    return Err("Browser composition unavailable");
                }
                CGPathAddRect(path, std::ptr::null(), area);
                CGPathAddRoundedRect(path, std::ptr::null(), cut.rect, cut.radius, cut.radius);
                let shape: Retained<AnyObject> = msg_send![class!(CAShapeLayer), new];
                let _: () = msg_send![&*shape,setFrame:area];
                let _: () = msg_send![&*shape,setPath:path];
                let _: () = msg_send![&*shape,setFillRule:&*NSString::from_str("even-odd")];
                if let Some(previous) = mask {
                    let _: () = msg_send![&*shape,setMask:&*previous];
                }
                CGPathRelease(path);
                mask = Some(shape);
            }
            let _: () = msg_send![class!(CATransaction), begin];
            let _: () = msg_send![class!(CATransaction), setDisableActions: true];
            let _: () = msg_send![layer,setMask:mask.as_deref()];
            let _: () = msg_send![class!(CATransaction), commit];
            Ok(())
        })()
        .map_err(str::to_owned);
        let _ = tx.send(result);
    })
    .map_err(|_| "Browser composition unavailable")?;
    tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .map_err(|_| "Browser composition timed out")?
        .map_err(|_| "Browser composition unavailable")?
}
