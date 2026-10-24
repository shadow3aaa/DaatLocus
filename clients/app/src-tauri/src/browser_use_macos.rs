//! WKWebView snapshots and responder-local AppKit input; never post global events.
use daat_locus_client_protocol::web::BrowserInput;
use objc2::{ClassType, msg_send, sel};
use objc2::{MainThreadMarker, rc::Retained};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSEvent, NSEventModifierFlags, NSEventType, NSImage,
    NSResponder, NSView,
};
use objc2_foundation::{NSDictionary, NSError, NSPoint, NSRange, NSRect, NSString};
use objc2_web_kit::WKWebView;
use objc2_web_kit::{WKWebViewConfiguration, WKWebsiteDataStore};
use std::{ffi::c_void, sync::Mutex, time::Duration};
use tauri::Webview;
#[path = "browser_layers_macos.rs"]
mod layers;
pub(crate) use layers::occlude;
thread_local! {static STORES:std::cell::RefCell<std::collections::HashMap<String,Retained<WKWebsiteDataStore>>>=std::cell::RefCell::new(Default::default());}
thread_local! {static SHORTCUTS:std::cell::RefCell<std::collections::HashMap<String,Retained<objc2::runtime::AnyObject>>>=std::cell::RefCell::new(Default::default());}
pub(crate) fn remove_shortcuts(label: &str) {
    layers::remove(label);
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
    let (sender, receiver) = tokio::sync::oneshot::channel();
    window
        .with_webview(move |platform| unsafe {
            let web = &*platform.inner().cast::<WKWebView>();
            let _ = sender.send(dispatch_input(web, input));
        })
        .map_err(|_| "WKWebView event loop unavailable")?;
    tokio::time::timeout(Duration::from_secs(8), receiver)
        .await
        .map_err(|_| "Browser input timed out; observe before continuing")?
        .map_err(|_| "Browser input interrupted; do not replay")?
}
fn dispatch_input(web: &WKWebView, input: BrowserInput) -> Result<(), String> {
    unsafe {
        let view: &NSView = web;
        let native_window = view.window().ok_or("Browser window closed")?;

        let point = |x: f64, y: f64| -> NSPoint {
            let bounds: NSRect = msg_send![view, bounds];
            let flipped: bool = msg_send![view, isFlipped];
            let local = NSPoint::new(x, if flipped { y } else { bounds.size.height - y });
            let result: NSPoint =
                msg_send![view,convertPoint:local toView:std::ptr::null::<NSView>()];
            result
        };
        match input {
            BrowserInput::Move { x, y } => {
                let event=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(NSEventType::MouseMoved,point(x,y),NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,0,0.0).ok_or("Pointer event unavailable")?;
                view.mouseMoved(&event);
            }
            BrowserInput::Drag { x, y, to_x, to_y } => {
                let p = point(x, y);
                let local: NSPoint =
                    msg_send![view,convertPoint:p fromView:std::ptr::null::<NSView>()];
                let parent = view.superview();
                let hit: NSPoint = msg_send![view,convertPoint:local toView:parent.as_deref()];
                let target: *mut NSView = msg_send![view,hitTest:hit];
                let target = target.as_ref().ok_or("No browser drag receiver")?;
                let event = |kind, point, pressure| {
                    NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(kind,point,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,1,pressure).ok_or_else(||"Drag event unavailable".to_owned())
                };
                target.mouseDown(&event(NSEventType::LeftMouseDown, p, 1.0)?);
                for step in 1..=12 {
                    let t = f64::from(step) / 12.0;
                    target.mouseDragged(&event(
                        NSEventType::LeftMouseDragged,
                        point(x + (to_x - x) * t, y + (to_y - y) * t),
                        1.0,
                    )?);
                }
                target.mouseUp(&event(NSEventType::LeftMouseUp, point(to_x, to_y), 0.0)?);
            }
            BrowserInput::Click {
                x,
                y,
                button,
                count,
            } => {
                let p = point(x, y);
                let local: NSPoint =
                    msg_send![view,convertPoint:p fromView:std::ptr::null::<NSView>()];
                let parent = view.superview();
                let hit_point: NSPoint =
                    msg_send![view,convertPoint:local toView:parent.as_deref()];
                let target: *mut NSView = msg_send![view,hitTest:hit_point];
                let target = target.as_ref().ok_or("No browser receiver at this point")?;
                let _: bool = msg_send![&*native_window,makeFirstResponder:target];
                for click in 1..=count {
                    let right = button == "right";
                    let middle = button == "middle";
                    let down=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(if middle{NSEventType::OtherMouseDown}else if right{NSEventType::RightMouseDown}else{NSEventType::LeftMouseDown},p,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,click.into(),1.0).ok_or("Mouse event unavailable")?;
                    let up=NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(if middle{NSEventType::OtherMouseUp}else if right{NSEventType::RightMouseUp}else{NSEventType::LeftMouseUp},p,NSEventModifierFlags::empty(),0.0,native_window.windowNumber(),None,0,click.into(),0.0).ok_or("Mouse event unavailable")?;
                    if middle {
                        let _: () = msg_send![target,otherMouseDown:&*down];
                        let _: () = msg_send![target,otherMouseUp:&*up];
                    } else if right {
                        let _: () = msg_send![target,rightMouseDown:&*down];
                        let _: () = msg_send![target,rightMouseUp:&*up];
                    } else {
                        target.mouseDown(&down);
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
                CGEventSetLocation(event, point(x, y));
                let native: *mut NSEvent = msg_send![NSEvent::class(),eventWithCGEvent:event];
                if let Some(native) = native.as_ref() {
                    view.scrollWheel(native);
                }
                CFRelease(event);
                if native.is_null() {
                    return Err("Scroll event unavailable".into());
                }
            }
            BrowserInput::Type { text } => {
                let responder = page_responder(view)?;
                let supported: bool =
                    msg_send![&*responder,respondsToSelector:sel!(insertText:replacementRange:)];
                if !supported {
                    return Err("Focus an editable browser field before typing".into());
                }
                let text = NSString::from_str(&text);
                let _: () = msg_send![&*responder,insertText:&*text replacementRange:NSRange::new(isize::MAX as _,0)];
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
                let (code, chars) = key_info(&key)?;
                let chars = NSString::from_str(&chars);
                for event_type in [NSEventType::KeyDown, NSEventType::KeyUp] {
                    let event=NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(event_type,NSPoint::new(0.0,0.0),flags,0.0,native_window.windowNumber(),None,&chars,&chars,false,code).ok_or("Key event unavailable")?;
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
fn page_responder(view: &NSView) -> Result<Retained<NSResponder>, String> {
    let window = view.window().ok_or("Browser window closed")?;
    let responder = window
        .firstResponder()
        .ok_or("Focus a browser field first")?;
    // A sibling chrome/hidden tab must never receive this page's input.
    let belongs = unsafe {
        let is_view: bool = msg_send![&*responder,isKindOfClass:NSView::class()];
        is_view && {
            let child: bool = msg_send![&*responder,isDescendantOf:view];
            child
        }
    };
    if !belongs {
        return Err("Focus a field in this browser page before typing".into());
    }
    Ok(responder)
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
        .find(|(ch, _)| Some(*ch) == key.to_ascii_lowercase().chars().next())
        .filter(|_| key.len() == 1)
        .map(|(_, code)| (*code, key.into()))
        .ok_or("Unsupported browser key".into())
}
