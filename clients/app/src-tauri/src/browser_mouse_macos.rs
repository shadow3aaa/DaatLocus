//! Scoped, process-local button state for native WebKit input.
// WebKit reads +[NSEvent pressedMouseButtons], not the synthesized event's
// buttons. Match WebKit's own AppKit automation approach without pressing any
// physical buttons. Other threads retain the original implementation's result.
use objc2::{
    ClassType,
    runtime::{AnyClass, Imp, Method, Sel},
    sel,
};
use objc2_app_kit::NSEvent;
use objc2_foundation::MainThreadMarker;
use std::{cell::Cell, sync::OnceLock};

type Original = unsafe extern "C-unwind" fn(&AnyClass, Sel) -> usize;
static ORIGINAL: OnceLock<Original> = OnceLock::new();
thread_local! {static BUTTONS: Cell<Option<usize>> = const { Cell::new(None) };}
unsafe extern "C-unwind" fn pressed(class: &AnyClass, selector: Sel) -> usize {
    BUTTONS.with(|buttons| buttons.get()).unwrap_or_else(|| {
        // Initialized before installing this method, including for other threads.
        unsafe { ORIGINAL.get().unwrap()(class, selector) }
    })
}
pub(crate) struct Buttons {
    method: &'static Method,
    original: Imp,
    previous: Option<usize>,
}
impl Buttons {
    pub(crate) fn new() -> Result<Self, String> {
        MainThreadMarker::new().ok_or("Browser input requires the native main thread")?;
        let method = NSEvent::class()
            .class_method(sel!(pressedMouseButtons))
            .ok_or("Native button state unavailable")?;
        ORIGINAL.get_or_init(|| unsafe {
            std::mem::transmute::<Imp, Original>(method.implementation())
        });
        let previous = BUTTONS.with(|buttons| buttons.replace(Some(0)));
        // Same NSUInteger return and (Class, SEL) arguments as the original.
        let original =
            unsafe { method.set_implementation(std::mem::transmute::<Original, Imp>(pressed)) };
        Ok(Self {
            method,
            original,
            previous,
        })
    }
    pub(crate) fn set(&self, buttons: usize) {
        BUTTONS.with(|value| value.set(Some(buttons)));
    }
}
impl Drop for Buttons {
    fn drop(&mut self) {
        unsafe {
            self.method.set_implementation(self.original);
        }
        BUTTONS.with(|buttons| buttons.set(self.previous));
    }
}

// NSTextInputContext may enqueue a pointer event after the caller has already
// queued a wheel. Hover never edits text: suppress IME mediation for this exact
// page and synchronous dispatch only. Owner/keyboard events retain their context.
type InputContext =
    unsafe extern "C-unwind" fn(&objc2::runtime::AnyObject, Sel) -> *mut objc2::runtime::AnyObject;
static INPUT_CONTEXT: OnceLock<InputContext> = OnceLock::new();
thread_local! {static POINTER_TARGET: Cell<usize> = const { Cell::new(0) };}
unsafe extern "C-unwind" fn pointer_context(
    view: &objc2::runtime::AnyObject,
    selector: Sel,
) -> *mut objc2::runtime::AnyObject {
    if POINTER_TARGET.with(|target| target.get()) == std::ptr::from_ref(view) as usize {
        std::ptr::null_mut()
    } else {
        unsafe { INPUT_CONTEXT.get().unwrap()(view, selector) }
    }
}
pub(crate) struct PointerContext {
    method: &'static Method,
    original: Imp,
    previous: usize,
}
impl PointerContext {
    pub(crate) fn new(view: &objc2_web_kit::WKWebView) -> Result<Self, String> {
        MainThreadMarker::new().ok_or("Browser pointer input requires the native main thread")?;
        let method = objc2_web_kit::WKWebView::class()
            .instance_method(sel!(inputContext))
            .ok_or("Native pointer input context unavailable")?;
        INPUT_CONTEXT.get_or_init(|| unsafe {
            std::mem::transmute::<Imp, InputContext>(method.implementation())
        });
        let previous =
            POINTER_TARGET.with(|target| target.replace(std::ptr::from_ref(view) as usize));
        let original = unsafe {
            method.set_implementation(std::mem::transmute::<InputContext, Imp>(pointer_context))
        };
        Ok(Self {
            method,
            original,
            previous,
        })
    }
}
impl Drop for PointerContext {
    fn drop(&mut self) {
        unsafe {
            self.method.set_implementation(self.original);
        }
        POINTER_TARGET.with(|target| target.set(self.previous));
    }
}
