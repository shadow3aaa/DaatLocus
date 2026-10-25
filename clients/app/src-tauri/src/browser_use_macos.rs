//! WKWebView snapshots and responder-local AppKit input; never post global events.
use daat_locus_client_protocol::web::BrowserInput;
use objc2::{ClassType, msg_send, sel};
use objc2::{MainThreadMarker, MainThreadOnly, rc::Retained};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags, NSEventType, NSImage,
    NSResponder, NSView,
};
use objc2_foundation::{
    NSDictionary, NSError, NSObjectProtocol, NSPoint, NSRange, NSRect, NSString,
};
use objc2_web_kit::WKWebView;
use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
use std::{ffi::c_void, sync::Mutex, time::Duration};
use tauri::Webview;
#[path = "browser_keys_macos.rs"]
mod keys;
#[path = "browser_layers_macos.rs"]
mod layers;
#[path = "browser_mouse_macos.rs"]
mod mouse;
#[path = "browser_wk_macos.rs"]
mod wk;
pub(crate) use layers::compose;
pub(crate) use wk::{find, prepare};
#[cfg(feature = "web-contract")]
pub(crate) fn ui_probe_visibility(view: &Webview) {
    let _ = view.with_webview(|platform| unsafe {
        let web = &*platform.inner().cast::<WKWebView>();
        // Exercise foreground UI while leaving the user's actual key window
        // alone. Occlusion would otherwise suspend requestAnimationFrame.
        if web.respondsToSelector(sel!(_setWindowOcclusionDetectionEnabled:)) {
            let _: () = msg_send![web, _setWindowOcclusionDetectionEnabled:false];
        }
        if let Some(window) = web.window() {
            window.setIgnoresMouseEvents(true);
            window.orderFrontRegardless();
        }
    });
}
thread_local! {static STORES:std::cell::RefCell<std::collections::HashMap<String,Retained<WKWebsiteDataStore>>>=std::cell::RefCell::new(Default::default());}
thread_local! {static SHORTCUTS:std::cell::RefCell<std::collections::HashMap<String,Retained<objc2::runtime::AnyObject>>>=std::cell::RefCell::new(Default::default());}
pub(crate) fn remove_shortcuts(label: &str) {
    layers::remove(label);
    wk::remove(label);
    SHORTCUTS.with(|monitors| {
        if let Some(monitor) = monitors.borrow_mut().remove(label) {
            unsafe {
                NSEvent::removeMonitor(&monitor);
            }
        }
    });
}
pub(crate) fn shortcuts(view: &Webview, app: tauri::AppHandle) {
    let label = view.label().to_owned();
    let _=view.with_webview(move |platform|unsafe{
        use tauri::Manager;
        let Some(web)=Retained::retain(platform.inner().cast::<WKWebView>())else{return};
        let callback=block2::RcBlock::new(move |event:std::ptr::NonNull<NSEvent>|->*mut NSEvent{
            let event_ref=event.as_ref();
            if keys::synthetic(event_ref){return event.as_ptr();}
            let Some(window)=web.window()else{return event.as_ptr()};
            if event_ref.windowNumber()!=window.windowNumber(){return event.as_ptr();}
            if web.isHiddenOrHasHiddenAncestor()||page_responder(&web).is_err(){return event.as_ptr();}
            let flags=event_ref.modifierFlags();let shift=flags.contains(NSEventModifierFlags::Shift);
            let command=flags.intersects(NSEventModifierFlags::Command|NSEventModifierFlags::Control);
            let alt=flags.contains(NSEventModifierFlags::Option);let key=event_ref.keyCode();
            let chars=event_ref.charactersIgnoringModifiers().map(|value|value.to_string().to_lowercase()).unwrap_or_default();
            let code:Option<String>=if command&&!alt{
                match key{
                    48=>Some(if shift{"previous-tab".into()}else{"next-tab".into()}),
                    24=>Some("zoom-in".into()),
                    _ if chars=="g"=>Some(if shift{"find-previous".into()}else{"find-next".into()}),
                    _ if shift=>None,
                    116=>Some("previous-tab".into()),121=>Some("next-tab".into()),27=>Some("zoom-out".into()),
                    _ if chars=="0"=>Some("zoom-reset".into()),
                    _ if matches!(chars.as_str(),"1"|"2"|"3"|"4"|"5"|"6"|"7"|"8"|"9")=>Some(format!("tab-{chars}")),
                    _ if matches!(chars.as_str(),"l"|"t"|"h"|"j"|"f"|"w"|"r"|"p")=>Some(chars),_=>None,
                }
            }else if alt&&!command&&!shift{match key{123=>Some("back".into()),124=>Some("forward".into()),_ if chars=="d"=>Some("l".into()),_=>None}}
            else if !command&&!alt{match key{96 if !shift=>Some("r".into()),99=>Some(if shift{"find-previous".into()}else{"find-next".into()}),_=>None}}else{None};
            if let Some(code)=code&&let Some(main)=app.get_webview("main"){
                let _=main.set_focus();let _=main.eval(format!("window.dispatchEvent(new CustomEvent('dl-browser-shortcut',{{detail:{}}}))",serde_json::to_string(&code).unwrap()));
                std::ptr::null_mut()
            }else{event.as_ptr()}
        });
        if let Some(monitor)=NSEvent::addLocalMonitorForEventsMatchingMask_handler(objc2_app_kit::NSEventMask::KeyDown,&callback){
            remove_shortcuts(&label);SHORTCUTS.with(|monitors|{monitors.borrow_mut().insert(label,monitor);});
        }
    });
}
pub(crate) fn configuration(profile: &str) -> Result<Retained<WKWebViewConfiguration>, String> {
    let main =
        MainThreadMarker::new().ok_or("WebKit configuration requires the native main thread")?;
    // The shared browser profile reuses one memory-only store across every tab.
    STORES.with(|stores| unsafe {
        let mut stores = stores.borrow_mut();
        let store = stores
            .entry(profile.into())
            .or_insert_with(|| WKWebsiteDataStore::nonPersistentDataStore(main));
        let configuration = WKWebViewConfiguration::new(main);
        configuration.setWebsiteDataStore(store);
        Ok(configuration)
    })
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGEventCreateScrollWheelEvent(
        source: *const c_void,
        units: u32,
        count: u32,
        wheel1: i32,
        ...
    ) -> *mut c_void;
    fn CGEventSetLocation(event: *mut c_void, point: NSPoint);
    fn CGEventSetIntegerValueField(event: *mut c_void, field: u32, value: i64);
}
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(value: *const c_void);
}

pub async fn screenshot(window: &Webview) -> Result<String, String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |platform| unsafe {
            let web = &*platform.inner().cast::<WKWebView>();
            let sender = Mutex::new(Some(sender));
            let completion =
                block2::RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
                    let result = (|| -> Result<String, String> {
                        if !error.is_null() {
                            return Err("WKWebView screenshot failed".into());
                        }
                        let image = image.as_ref().ok_or("WKWebView screenshot missing")?;
                        let tiff = image
                            .TIFFRepresentation()
                            .ok_or("WKWebView pixels unavailable")?;
                        let rep = NSBitmapImageRep::imageRepWithData(&tiff)
                            .ok_or("WKWebView image unavailable")?;
                        let png = rep
                            .representationUsingType_properties(
                                NSBitmapImageFileType::PNG,
                                &NSDictionary::new(),
                            )
                            .ok_or("WKWebView PNG unavailable")?;
                        use base64::Engine;
                        Ok(base64::engine::general_purpose::STANDARD.encode(png.to_vec()))
                    })();
                    if let Some(sender) = sender.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        let _ = sender.send(result);
                    }
                });
            web.takeSnapshotWithConfiguration_completionHandler(None, &completion);
        })
        .map_err(|_| "WKWebView event loop unavailable")?;
    tokio::time::timeout(Duration::from_secs(8), receiver)
        .await
        .map_err(|_| "WKWebView screenshot timed out")?
        .map_err(|_| "WKWebView screenshot interrupted")?
}
pub async fn navigation(window: &Webview) -> Result<(bool, bool), String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |platform| unsafe {
            let web = &*platform.inner().cast::<WKWebView>();
            let _ = sender.send((web.canGoBack(), web.canGoForward()));
        })
        .map_err(|_| "Browser history unavailable")?;
    receiver
        .await
        .map_err(|_| "Browser history unavailable".into())
}
pub async fn input(window: &Webview, input: BrowserInput) -> Result<(), String> {
    let editing = keys::editing(&input);
    if let Some(editing) = &editing {
        keys::begin(window, editing).await?;
    }
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |platform| unsafe {
            let web = &*platform.inner().cast::<WKWebView>();
            #[cfg(feature = "web-contract")]
            let before = physical_input_state();
            let result = dispatch_input(web, input);
            #[cfg(feature = "web-contract")]
            let result = if before != physical_input_state() {
                Err("Native browser input changed OS focus, pointer or button state".into())
            } else {
                result
            };
            let _ = sender.send(result);
        })
        .map_err(|_| "WKWebView event loop unavailable")?;
    tokio::time::timeout(Duration::from_secs(8), receiver)
        .await
        .map_err(|_| "Browser input timed out; observe before continuing")?
        .map_err(|_| "Browser input interrupted; do not replay")??;
    if let Some(editing) = editing {
        keys::finish(window, editing).await?;
    }
    Ok(())
}
#[cfg(feature = "web-contract")]
unsafe fn physical_input_state() -> (NSPoint, isize, i32, usize) {
    use objc2::{class, runtime::AnyObject};
    unsafe {
        let app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let key: *mut AnyObject = msg_send![app, keyWindow];
        let key = if key.is_null() {
            0
        } else {
            msg_send![key, windowNumber]
        };
        let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
        let front: *mut AnyObject = msg_send![workspace, frontmostApplication];
        let pid = if front.is_null() {
            0
        } else {
            msg_send![front, processIdentifier]
        };
        (
            NSEvent::mouseLocation(),
            key,
            pid,
            NSEvent::pressedMouseButtons(),
        )
    }
}
fn dispatch_input(web: &WKWebView, input: BrowserInput) -> Result<(), String> {
    unsafe {
        let view: &NSView = web;
        let native_window = view.window().ok_or("Browser window closed")?;
        let buttons = mouse::Buttons::new()?;

        let point = |x: f64, y: f64| -> NSPoint {
            let bounds: NSRect = msg_send![view, bounds];
            let flipped: bool = msg_send![view, isFlipped];
            let local = NSPoint::new(x, if flipped { y } else { bounds.size.height - y });
            let result: NSPoint =
                msg_send![view,convertPoint:local, toView:std::ptr::null::<NSView>()];
            result
        };
        match input {
            BrowserInput::Move { x, y } => {
                // WebKit routes an inactive page's MouseMoved only to scrollbars.
                // A local drag-movement event also reaches content. Without a
                // preceding down event this remains hover (buttons == 0), and
                // never starts a drag or changes the physical button state.
                let event=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::LeftMouseDragged,point(x,y),NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,0,0.0).ok_or("Pointer event unavailable")?;
                // WebKit tracks movement through its tracking observer rather than
                // NSResponder::mouseMoved. Its view-local test entry point keeps
                // hover trusted without posting an event or moving the OS cursor.
                if !web.respondsToSelector(sel!(_simulateMouseMove:)) {
                    return Err("Native browser hover requires macOS 13 or newer".into());
                }
                let _: () = msg_send![web, _simulateMouseMove: &*event];
            }
            BrowserInput::Drag { x, y, to_x, to_y } => {
                let p = point(x, y);
                let local: NSPoint =
                    msg_send![view,convertPoint:p, fromView:std::ptr::null::<NSView>()];
                let parent = view.superview();
                let hit: NSPoint = msg_send![view,convertPoint:local, toView:parent.as_deref()];
                let target: *mut NSView = msg_send![view,hitTest:hit];
                let target = target.as_ref().ok_or("No browser drag receiver")?;
                let event = |kind, point, pressure| {
                    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind,point,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,1,pressure).ok_or_else(||"Drag event unavailable".to_owned())
                };
                buttons.set(1);
                target.mouseDown(&*event(NSEventType::LeftMouseDown, p, 1.0)?);
                for step in 1..=12 {
                    let t = f64::from(step) / 12.0;
                    target.mouseDragged(&*event(
                        NSEventType::LeftMouseDragged,
                        point(x + (to_x - x) * t, y + (to_y - y) * t),
                        1.0,
                    )?);
                }
                buttons.set(0);
                target.mouseUp(&*event(NSEventType::LeftMouseUp, point(to_x, to_y), 0.0)?);
            }
            BrowserInput::Click {
                x,
                y,
                button,
                count,
            } => {
                let p = point(x, y);
                let local: NSPoint =
                    msg_send![view,convertPoint:p, fromView:std::ptr::null::<NSView>()];
                let parent = view.superview();
                let hit_point: NSPoint =
                    msg_send![view,convertPoint:local, toView:parent.as_deref()];
                let target: *mut NSView = msg_send![view,hitTest:hit_point];
                let target = target.as_ref().ok_or("No browser receiver at this point")?;
                let _: bool = msg_send![&*native_window,makeFirstResponder:target];
                for click in 1..=count {
                    let right = button == "right";
                    let middle = button == "middle";
                    let down=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(if middle{NSEventType::OtherMouseDown}else if right{NSEventType::RightMouseDown}else{NSEventType::LeftMouseDown},p,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,click.into(),1.0).ok_or("Mouse event unavailable")?;
                    let up=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(if middle{NSEventType::OtherMouseUp}else if right{NSEventType::RightMouseUp}else{NSEventType::LeftMouseUp},p,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,click.into(),0.0).ok_or("Mouse event unavailable")?;
                    let (down, up) = if middle {
                        (middle_button(&down)?, middle_button(&up)?)
                    } else {
                        (down, up)
                    };
                    buttons.set(if middle {
                        4
                    } else if right {
                        2
                    } else {
                        1
                    });
                    if middle {
                        let _: () = msg_send![target,otherMouseDown:&*down];
                        buttons.set(0);
                        let _: () = msg_send![target,otherMouseUp:&*up];
                    } else if right {
                        let _: () = msg_send![target,rightMouseDown:&*down];
                        buttons.set(0);
                        let _: () = msg_send![target,rightMouseUp:&*up];
                    } else {
                        target.mouseDown(&down);
                        buttons.set(0);
                        target.mouseUp(&up);
                    }
                }
            }
            BrowserInput::Scroll {
                x,
                y,
                delta_x,
                delta_y,
            } => {
                let event = CGEventCreateScrollWheelEvent(
                    std::ptr::null(),
                    0,
                    2,
                    (-delta_y).round() as i32,
                    (-delta_x).round() as i32,
                );
                if event.is_null() {
                    return Err("Scroll event unavailable".into());
                }
                // CGEvent uses global, top-down screen coordinates. Rebind the
                // NSEvent to this window before WebKit hit-tests nested scrollers.
                let screens: *mut objc2::runtime::AnyObject =
                    msg_send![objc2::class!(NSScreen), screens];
                let screen: *mut objc2::runtime::AnyObject = msg_send![screens, firstObject];
                let frame: NSRect = msg_send![screen, frame];
                let location = native_window.convertPointToScreen(point(x, y));
                CGEventSetLocation(
                    event,
                    NSPoint::new(location.x, frame.size.height - location.y),
                );
                let native: Option<Retained<NSEvent>> =
                    msg_send![NSEvent::class(),eventWithCGEvent:event];
                CFRelease(event);
                let native = native.ok_or("Scroll event unavailable")?;
                if !native.respondsToSelector(sel!(_eventRelativeToWindow:)) {
                    return Err("Window-local scroll events unavailable".into());
                }
                let local: Option<Retained<NSEvent>> =
                    msg_send![&*native, _eventRelativeToWindow:&*native_window];
                view.scrollWheel(&*local.ok_or("Window-local scroll event unavailable")?);
            }
            BrowserInput::Type { text } => {
                let responder = page_responder(view)?;
                let supported: bool =
                    msg_send![&*responder,respondsToSelector:sel!(insertText:replacementRange:)];
                if !supported {
                    return Err("Focus an editable browser field before typing".into());
                }
                let text = NSString::from_str(&text);
                let _: () = msg_send![&*responder,insertText:&*text, replacementRange:NSRange::new(isize::MAX as _,0)];
            }
            BrowserInput::Key { key, modifiers } => {
                let responder = page_responder(view)?;
                let flags = modifiers
                    .iter()
                    .fold(NSEventModifierFlags::empty(), |flags, m| {
                        flags
                            | match m.as_str() {
                                "control" => NSEventModifierFlags::Control,
                                "shift" => NSEventModifierFlags::Shift,
                                "alt" => NSEventModifierFlags::Option,
                                "meta" => NSEventModifierFlags::Command,
                                _ => NSEventModifierFlags::empty(),
                            }
                    });
                let (code, mut chars) = key_info(&key)?;
                // AppKit key equivalents are case-sensitive. "Command+A"
                // names the A key; uppercase characters without Shift would
                // instead miss the standard select-all binding.
                if flags.intersects(NSEventModifierFlags::Command | NSEventModifierFlags::Control)
                    && !flags.contains(NSEventModifierFlags::Shift)
                {
                    chars.make_ascii_lowercase();
                }
                let chars = NSString::from_str(&chars);
                for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
                    let event=NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(event_type,NSPoint::new(0.0,0.0),flags,0.0,native_window.windowNumber(),None,&chars,&chars,false,code).ok_or("Key event unavailable")?;
                    keys::tag(&event, web)?;
                    if event_type == NSEventType::KeyDown {
                        responder.keyDown(&event)
                    } else {
                        responder.keyUp(&event)
                    }
                }
            }
            BrowserInput::Back => {
                if !web.canGoBack() {
                    return Err("No previous browser page".into());
                }
                let _ = web.goBack();
            }
            BrowserInput::Forward => {
                if !web.canGoForward() {
                    return Err("No next browser page".into());
                }
                let _ = web.goForward();
            }
            BrowserInput::Reload => {
                let _ = web.reload();
            }
            BrowserInput::Navigate { .. } => {
                return Err("Navigation is dispatched by the host".into());
            }
        }
        Ok(())
    }
}
unsafe fn middle_button(event: &NSEvent) -> Result<Retained<NSEvent>, String> {
    unsafe {
        // AppKit's OtherMouse factory does not select the middle button.
        let cg: *mut c_void = msg_send![event, CGEvent];
        if cg.is_null() {
            return Err("Native middle-button event unavailable".into());
        }
        const MOUSE_BUTTON_NUMBER: u32 = 3;
        CGEventSetIntegerValueField(cg, MOUSE_BUTTON_NUMBER, 2);
        let event: Option<Retained<NSEvent>> = msg_send![NSEvent::class(), eventWithCGEvent:cg];
        event.ok_or_else(|| "Native middle-button event unavailable".into())
    }
}
fn page_responder(view: &NSView) -> Result<Retained<NSResponder>, String> {
    let window = view.window().ok_or("Browser window closed")?;
    if let Some(responder) = window.firstResponder() {
        let belongs = unsafe {
            let is_view: bool = msg_send![&*responder,isKindOfClass:NSView::class()];
            is_view && {
                let child: bool = msg_send![&*responder,isDescendantOf:view];
                child
            }
        };
        if belongs {
            return Ok(responder);
        }
    }
    // WebKit keeps DOM focus per page even when its native view moves between
    // its private background window and the Owner's visible viewport.
    let mut pending = view.subviews().to_vec();
    while let Some(child) = pending.pop() {
        if child.respondsToSelector(sel!(insertText:replacementRange:)) {
            return Ok(child.into_super());
        }
        pending.extend(child.subviews().to_vec());
    }
    Err("Focus an editable field in this browser page before typing".into())
}
fn key_info(key: &str) -> Result<(u16, String), String> {
    let special = match key {
        "Enter" => Some((36, "\r")),
        "Tab" => Some((48, "\t")),
        "Space" => Some((49, " ")),
        "F3" => Some((99, "\u{f706}")),
        "F5" => Some((96, "\u{f708}")),
        "+" => Some((24, "+")),
        "=" => Some((24, "=")),
        "-" => Some((27, "-")),
        "Backspace" => Some((51, "\u{7f}")),
        "Escape" => Some((53, "\u{1b}")),
        "Delete" => Some((117, "\u{f728}")),
        "ArrowLeft" => Some((123, "\u{f702}")),
        "ArrowRight" => Some((124, "\u{f703}")),
        "ArrowDown" => Some((125, "\u{f701}")),
        "ArrowUp" => Some((126, "\u{f700}")),
        "Home" => Some((115, "\u{f729}")),
        "End" => Some((119, "\u{f72b}")),
        "PageUp" => Some((116, "\u{f72c}")),
        "PageDown" => Some((121, "\u{f72d}")),
        _ => None,
    };
    if let Some((code, text)) = special {
        return Ok((code, text.into()));
    }
    let keys = [
        ('a', 0),
        ('s', 1),
        ('d', 2),
        ('f', 3),
        ('h', 4),
        ('g', 5),
        ('z', 6),
        ('x', 7),
        ('c', 8),
        ('v', 9),
        ('b', 11),
        ('q', 12),
        ('w', 13),
        ('e', 14),
        ('r', 15),
        ('y', 16),
        ('t', 17),
        ('1', 18),
        ('2', 19),
        ('3', 20),
        ('4', 21),
        ('6', 22),
        ('5', 23),
        ('9', 25),
        ('7', 26),
        ('8', 28),
        ('0', 29),
        ('o', 31),
        ('u', 32),
        ('i', 34),
        ('p', 35),
        ('l', 37),
        ('j', 38),
        ('k', 40),
        ('n', 45),
        ('m', 46),
    ];
    keys.iter()
        .find(|(ch, _)| key.to_ascii_lowercase().starts_with(*ch))
        .filter(|_| key.len() == 1)
        .map(|(_, code)| (*code, key.into()))
        .ok_or("Unsupported browser key".into())
}

pub async fn configure(view: &Webview, script: &str) -> Result<(), String> {
    let script = script.to_owned();
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move |platform| {
        unsafe {
            use objc2_web_kit::{WKUserScript, WKUserScriptInjectionTime};
            let web = &*platform.inner().cast::<WKWebView>();
            let content = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                WKUserScript::alloc(MainThreadMarker::new().expect("WebView main thread")),
                &NSString::from_str(&script),
                WKUserScriptInjectionTime::AtDocumentStart,
                false,
            );
            let controller = web.configuration().userContentController();
            let scripts = controller.userScripts();
            controller.removeAllUserScripts();
            for existing in scripts.iter() {
                if !existing
                    .source()
                    .to_string()
                    .starts_with("/*dl-browser-language*/")
                {
                    controller.addUserScript(&existing);
                }
            }
            controller.addUserScript(&content);
        }
        let _ = tx.send(());
    })
    .map_err(|_| "Browser language unavailable")?;
    rx.await.map_err(|_| "Browser language unavailable".into())
}

pub async fn navigate(view: &Webview, url: &url::Url, locale: &str) -> Result<(), String> {
    let url = url.to_string();
    let locale = locale.to_owned();
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move |platform| {
        use objc2_foundation::{NSMutableURLRequest, NSURL};
        let result = (|| unsafe {
            let url =
                NSURL::URLWithString(&NSString::from_str(&url)).ok_or("Invalid browser URL")?;
            let request = NSMutableURLRequest::requestWithURL(&url);
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(&locale)),
                &NSString::from_str("Accept-Language"),
            );
            let web = &*platform.inner().cast::<WKWebView>();
            web.loadRequest(&request)
                .ok_or("Browser navigation failed")?;
            Ok::<_, String>(())
        })();
        let _ = tx.send(result);
    })
    .map_err(|_| "Browser navigation failed")?;
    rx.await.map_err(|_| "Browser navigation failed")?
}

#[cfg(feature = "web-contract")]
pub(crate) fn input_focus() -> serde_json::Value {
    unsafe {
        let (point, key, pid, buttons) = physical_input_state();
        let app: *mut objc2::runtime::AnyObject =
            msg_send![objc2::class!(NSApplication), sharedApplication];
        let window: *mut objc2::runtime::AnyObject = msg_send![app, keyWindow];
        let responder: *mut objc2::runtime::AnyObject = if window.is_null() {
            std::ptr::null_mut()
        } else {
            msg_send![window, firstResponder]
        };
        serde_json::json!({"key":key,"application":pid,"responder":responder as usize,"pointer":[point.x,point.y],"buttons":buttons})
    }
}
