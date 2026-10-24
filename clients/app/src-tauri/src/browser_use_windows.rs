//! WebView2's native CDP interface, without a listening debugging port.
use daat_locus_client_protocol::web::BrowserInput;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tauri::Webview;
pub async fn favicon(view: &Webview) -> Result<String, String> {
    use webview2_com::{GetFaviconCompletedHandler, Microsoft::Web::WebView2::Win32::*};
    use windows::core::Interface;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let tx = Arc::new(Mutex::new(Some(tx)));
    let failed = tx.clone();
    view.with_webview(move |platform| {
        let result = (|| -> windows::core::Result<()> {
            unsafe {
                let core: ICoreWebView2_15 = platform.controller().CoreWebView2()?.cast()?;
                core.GetFavicon(
                    COREWEBVIEW2_FAVICON_IMAGE_FORMAT_PNG,
                    &GetFaviconCompletedHandler::create(Box::new(move |status, stream| {
                        let result = (|| -> Result<String, String> {
                            use base64::Engine;
                            status.map_err(|_| "Site icon unavailable")?;
                            let stream = stream.ok_or("Site icon unavailable")?;
                            let mut bytes = vec![0u8; 32_769];
                            let mut read = 0;
                            stream
                                .Read(
                                    bytes.as_mut_ptr().cast(),
                                    bytes.len() as u32,
                                    Some(&mut read),
                                )
                                .ok()
                                .map_err(|_| "Site icon unavailable")?;
                            bytes.truncate(read as usize);
                            if bytes.len() > 32_768 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                                return Err("Invalid site icon".into());
                            }
                            Ok(format!(
                                "data:image/png;base64,{}",
                                base64::engine::general_purpose::STANDARD.encode(bytes)
                            ))
                        })();
                        if let Some(tx) = tx.lock().unwrap_or_else(|e| e.into_inner()).take() {
                            let _ = tx.send(result);
                        }
                        Ok(())
                    })),
                )
            }
        })();
        if result.is_err()
            && let Some(tx) = failed.lock().unwrap_or_else(|e| e.into_inner()).take()
        {
            let _ = tx.send(Err("Site icon unavailable".into()));
        }
    })
    .map_err(|_| "Site icon unavailable")?;
    tokio::time::timeout(Duration::from_secs(3), rx)
        .await
        .map_err(|_| "Site icon timed out")?
        .map_err(|_| "Site icon unavailable")?
}
pub fn shortcuts(view: &Webview, app: tauri::AppHandle) {
    let metadata = app.clone();
    let label = view.label().to_owned();
    let _ = view.with_webview(move |platform| unsafe {
        use tauri::Manager;
        use webview2_com::{FaviconChangedEventHandler, Microsoft::Web::WebView2::Win32::*};
        use windows::core::Interface;
        if let Ok(core) = platform
            .controller()
            .CoreWebView2()
            .and_then(|core| core.cast::<ICoreWebView2_15>())
        {
            let _ = core.add_FaviconChanged(
                &FaviconChangedEventHandler::create(Box::new(move |_, _| {
                    if let Some(host) = metadata.try_state::<crate::web_host::NativeHost>()
                        && let Some(view) = metadata.get_webview(&label)
                    {
                        let host = host.inner().clone();
                        let key = label.clone();
                        tauri::async_runtime::spawn(async move {
                            host.capture_icon(&view, &key).await;
                        });
                    }
                    Ok(())
                })),
                &mut 0,
            );
        }
    });
    let _=view.with_webview(move |platform|unsafe{
        use tauri::Manager;
        use webview2_com::{Microsoft::Web::WebView2::Win32::*,AcceleratorKeyPressedEventHandler};
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetKeyState,VK_CONTROL,VK_MENU,VK_SHIFT};
        let _=platform.controller().add_AcceleratorKeyPressed(&AcceleratorKeyPressedEventHandler::create(Box::new(move |_,args|{
            let Some(args)=args else{return Ok(())};
            let mut kind=COREWEBVIEW2_KEY_EVENT_KIND::default();args.KeyEventKind(&mut kind)?;
            if kind!=COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind!=COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {return Ok(());}
            let mut key=0;args.VirtualKey(&mut key)?;
            let control=GetKeyState(VK_CONTROL as i32)<0;
            let alt=GetKeyState(VK_MENU as i32)<0;
            let shift=GetKeyState(VK_SHIFT as i32)<0;
            let shortcut=if control && !alt {
                match key {
                    0x09=>Some(if shift {"previous-tab".into()}else{"next-tab".into()}),
                    0x47=>Some(if shift {"find-previous".into()}else{"find-next".into()}),
                    0xbb|0x6b=>Some("zoom-in".into()),
                    _ if shift=>None,
                    0x21=>Some("previous-tab".into()),0x22=>Some("next-tab".into()),
                    0xbd|0x6d=>Some("zoom-out".into()),0x30=>Some("zoom-reset".into()),
                    0x31..=0x39=>Some(format!("tab-{}",key-0x30)),
                    0x4c|0x54|0x57|0x48|0x4a|0x46|0x50|0x52=>Some(char::from_u32(key).unwrap().to_ascii_lowercase().to_string()),
                    _=>None,
                }
            }else if alt && !control && !shift {
                match key {0x25=>Some("back".into()),0x27=>Some("forward".into()),0x44=>Some("l".into()),_=>None}
            }else if !alt && !control {
                match key {0x74 if !shift=>Some("r".into()),0x72=>Some(if shift {"find-previous".into()}else{"find-next".into()}),_=>None}
            }else{None};
            if let Some(shortcut)=shortcut && let Some(main)=app.get_webview("main") {
                args.SetHandled(true)?;
                let _=main.set_focus();
                let _=main.eval(format!("window.dispatchEvent(new CustomEvent('dl-browser-shortcut',{{detail:{}}}))",serde_json::to_string(&shortcut).unwrap()));
            }
            Ok(())
        })),&mut 0);
    });
}

async fn cdp(window: &Webview, method: &str, parameters: Value) -> Result<Value, String> {
    let method = method.to_owned();
    let parameters = parameters.to_string();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let failure = sender.clone();
    window
        .with_webview(move |view| {
            let result = (|| -> windows::core::Result<()> {
                unsafe {
                    let core = view.controller().CoreWebView2()?;
                    core.CallDevToolsProtocolMethod(
                        &windows::core::HSTRING::from(method),
                        &windows::core::HSTRING::from(parameters),
                        &webview2_com::CallDevToolsProtocolMethodCompletedHandler::create(
                            Box::new(move |status, value| {
                                let result = status
                                    .map_err(|_| {
                                        "Native browser operation failed; observe before continuing"
                                            .to_owned()
                                    })
                                    .and_then(|_| {
                                        serde_json::from_str(&value)
                                            .map_err(|_| "Invalid native browser response".into())
                                    });
                                if let Some(sender) =
                                    sender.lock().unwrap_or_else(|e| e.into_inner()).take()
                                {
                                    let _ = sender.send(result);
                                }
                                Ok(())
                            }),
                        ),
                    )
                }
            })();
            if result.is_err()
                && let Some(sender) = failure.lock().unwrap_or_else(|e| e.into_inner()).take()
            {
                let _ = sender.send(Err("Native browser operation could not start".into()));
            }
        })
        .map_err(|_| "Native browser event loop unavailable")?;
    tokio::time::timeout(Duration::from_secs(8), receiver)
        .await
        .map_err(|_| "Native browser operation timed out; do not replay")?
        .map_err(|_| "Native browser operation interrupted; do not replay")?
}
pub async fn screenshot(window: &Webview) -> Result<String, String> {
    let result = cdp(
        window,
        "Page.captureScreenshot",
        json!({"format":"png","fromSurface":true,"captureBeyondViewport":false}),
    )
    .await?;
    result["data"]
        .as_str()
        .map(str::to_owned)
        .ok_or("Native browser screenshot missing".into())
}
pub async fn compose(
    shell: &Webview,
    bounds: Option<daat_locus_client_protocol::web::BrowserBounds>,
    foreground: Vec<daat_locus_client_protocol::web::BrowserHitRegion>,
) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    shell
        .with_webview(move |platform| {
            let result = (|| -> windows::core::Result<()> {
                unsafe {
                    use wry::native_composition::{InputRect, set_input_regions};
                    let mut hwnd = windows::Win32::Foundation::HWND::default();
                    platform.controller().ParentWindow(&mut hwnd)?;
                    let scale = bounds.map(|area| area.scale).unwrap_or(1.0);
                    let rect = |x: f64, y: f64, w: f64, h: f64| InputRect {
                        left: (x * scale).floor() as i32,
                        top: (y * scale).floor() as i32,
                        right: ((x + w) * scale).ceil() as i32,
                        bottom: ((y + h) * scale).ceil() as i32,
                    };
                    let viewport = bounds.map(|area| rect(area.x, area.y, area.width, area.height));
                    let regions = foreground
                        .into_iter()
                        .map(|area| rect(area.x, area.y, area.width, area.height))
                        .collect();
                    set_input_regions(hwnd.0 as isize, viewport, regions)
                }
            })()
            .map_err(|_| "Browser input routing unavailable".to_owned());
            let _ = tx.send(result);
        })
        .map_err(|_| "Browser input routing unavailable")?;
    rx.await.map_err(|_| "Browser input routing unavailable")?
}
#[cfg(feature = "web-contract")]
pub async fn owner_click(app: &tauri::AppHandle, x: f64, y: f64) -> Result<(), String> {
    use tauri::Manager;
    let window = app.get_window("main").ok_or("Probe window unavailable")?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    let app = app.clone();
    app.run_on_main_thread(move || unsafe {
        use windows::Win32::{Foundation::*, Graphics::Gdi::*, UI::WindowsAndMessaging::*};
        let hwnd = HWND(window.hwnd().unwrap().0);
        let _ = windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow(hwnd);
        let mut point = POINT {
            x: (x * scale) as i32,
            y: (y * scale) as i32,
        };
        let _ = ClientToScreen(hwnd, &mut point);
        // Exercise our actual native sibling hit tests without ever sending
        // mouse input to another app or depending on global desktop occlusion.
        let screen = LPARAM(((point.y as u32) << 16 | point.x as u32 & 0xffff) as isize);
        let mut candidate = GetTopWindow(Some(hwnd)).ok();
        let mut target = HWND::default();
        while let Some(child) = candidate {
            let mut rect = RECT::default();
            let _ = GetWindowRect(child, &mut rect);
            if IsWindowVisible(child).as_bool()
                && point.x >= rect.left
                && point.x < rect.right
                && point.y >= rect.top
                && point.y < rect.bottom
            {
                let hit = SendMessageW(child, WM_NCHITTEST, Some(WPARAM(0)), Some(screen));
                if hit.0 != HTTRANSPARENT as isize && hit.0 != HTNOWHERE as isize {
                    target = child;
                    break;
                }
            }
            candidate = GetWindow(child, GW_HWNDNEXT).ok();
        }
        if target.0.is_null() {
            let _ = tx.send(Err("No native child accepted input".to_owned()));
            return;
        }
        let _ = ScreenToClient(target, &mut point);
        let position = LPARAM(((point.y as u32) << 16 | point.x as u32 & 0xffff) as isize);
        SendMessageW(target, WM_MOUSEMOVE, Some(WPARAM(0)), Some(position));
        SendMessageW(target, WM_LBUTTONDOWN, Some(WPARAM(1)), Some(position));
        SendMessageW(target, WM_LBUTTONUP, Some(WPARAM(0)), Some(position));
        let _ = tx.send(Ok(()));
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|e| e.to_string())?
}
#[cfg(feature = "web-contract")]
pub async fn owner_text(app: &tauri::AppHandle, text: String) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.run_on_main_thread(move || unsafe {
        use windows::Win32::{
            Foundation::*,
            UI::{Input::KeyboardAndMouse::GetFocus, WindowsAndMessaging::*},
        };
        let focus = GetFocus();
        for unit in text.encode_utf16() {
            SendMessageW(
                focus,
                WM_CHAR,
                Some(WPARAM(usize::from(unit))),
                Some(LPARAM(0)),
            );
        }
        let _ = tx.send(());
    })
    .map_err(|e| e.to_string())?;
    rx.await.map_err(|e| e.to_string())
}
#[cfg(feature = "web-contract")]
pub async fn composition_probe(view: &Webview) -> Result<Value, String> {
    let scale = view.window().scale_factor().map_err(|e| e.to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.with_webview(move|platform|{let result=(||->windows::core::Result<Value>{unsafe{
        use windows::Win32::{Foundation::*,Graphics::Gdi::*,UI::WindowsAndMessaging::*};
        let mut hwnd=HWND::default();platform.controller().ParentWindow(&mut hwnd)?;
        let parent=GetParent(hwnd)?;
        let mut size=RECT::default();GetClientRect(parent,&mut size)?;
        #[link(name="user32")]
        unsafe extern "system" {fn PrintWindow(hwnd:*mut std::ffi::c_void,dc:*mut std::ffi::c_void,flags:u32)->i32;}
        let source=GetDC(None);let dc=CreateCompatibleDC(Some(source));
        let bitmap=CreateCompatibleBitmap(source,size.right,size.bottom);let previous=SelectObject(dc,bitmap.into());
        let captured=PrintWindow(parent.0,dc.0,3)!=0;
        let pixel=|x:f64,y:f64|{let c=GetPixel(dc,(x*scale) as i32,(y*scale) as i32).0;[c&255,(c>>8)&255,(c>>16)&255]};
        let pixels=[pixel(150.0,250.0),pixel(500.0,300.0),pixel(310.0,250.0),pixel(450.0,250.0)];
        SelectObject(dc,previous);let _=DeleteObject(bitmap.into());let _=DeleteDC(dc);ReleaseDC(None,source);
        Ok(json!({"composed":wry::native_composition::is_composed(hwnd.0 as isize),"captured":captured,"pixels":pixels}))
    }})();let _=tx.send(result.map_err(|e|e.to_string()));}).map_err(|e|e.to_string())?;
    rx.await.map_err(|e| e.to_string())?
}
pub async fn find(window: &Webview, action: &str, text: &str) -> Result<Value, String> {
    let action = action.to_owned();
    let text = text.to_owned();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let sender = Arc::new(Mutex::new(Some(sender)));
    let failure = sender.clone();
    let event_token = Arc::new(std::sync::atomic::AtomicI64::new(0));
    let cleanup_token = event_token.clone();
    window
        .with_webview(move |platform| {
            use webview2_com::Microsoft::Web::WebView2::Win32::*;
            use windows::core::Interface;
            fn status(find: &ICoreWebView2Find) -> windows::core::Result<Value> {
                let (mut index, mut count) = (0, 0);
                unsafe {
                    find.ActiveMatchIndex(&mut index)?;
                    find.MatchCount(&mut count)?;
                }
                Ok(json!({"found":count>0,"count":count,"index":if count>0 {index} else {0}}))
            }
            let result = (|| -> windows::core::Result<()> {
                unsafe {
                    let core = platform.controller().CoreWebView2()?;
                    let find = core.cast::<ICoreWebView2_28>()?.Find()?;
                    if action == "find_close" || text.is_empty() {
                        find.Stop()?;
                        if let Some(sender) = sender.lock().unwrap().take() {
                            let _ = sender.send(Ok(json!({"found":false,"count":0,"index":0})));
                        }
                    } else if action == "find" {
                        find.Stop()?;
                        let options = core
                            .cast::<ICoreWebView2_2>()?
                            .Environment()?
                            .cast::<ICoreWebView2Environment15>()?
                            .CreateFindOptions()?;
                        options.SetFindTerm(&windows::core::HSTRING::from(text))?;
                        options.SetIsCaseSensitive(false)?;
                        options.SetShouldMatchWord(false)?;
                        options.SetShouldHighlightAllMatches(true)?;
                        options.SetSuppressDefaultFindDialog(true)?;
                        let current = find.clone();
                        find.Start(
                            &options,
                            &webview2_com::FindStartCompletedHandler::create(Box::new(
                                move |result| {
                                    let value = result
                                        .and_then(|_| status(&current))
                                        .map_err(|_| "Page search failed".to_string());
                                    if let Some(sender) = sender.lock().unwrap().take() {
                                        let _ = sender.send(value);
                                    }
                                    Ok(())
                                },
                            )),
                        )?;
                    } else {
                        let initial = status(&find)?;
                        if initial["count"].as_i64().unwrap_or(0) <= 1 {
                            if let Some(sender) = sender.lock().unwrap().take() {
                                let _ = sender.send(Ok(initial));
                            }
                        } else {
                            let current = find.clone();
                            let registered = event_token.clone();
                            let mut registration = 0;
                            find.add_ActiveMatchIndexChanged(
                                &webview2_com::FindActiveMatchIndexChangedEventHandler::create(
                                    Box::new(move |_, _| {
                                        let value = status(&current)
                                            .map_err(|_| "Page search failed".to_string());
                                        let _ = current.remove_ActiveMatchIndexChanged(
                                            registered
                                                .swap(0, std::sync::atomic::Ordering::Relaxed),
                                        );
                                        if let Some(sender) = sender.lock().unwrap().take() {
                                            let _ = sender.send(value);
                                        }
                                        Ok(())
                                    }),
                                ),
                                &mut registration,
                            )?;
                            event_token.store(registration, std::sync::atomic::Ordering::Relaxed);
                            if action == "find_previous" {
                                find.FindPrevious()?;
                            } else {
                                find.FindNext()?;
                            }
                        }
                    }
                    Ok(())
                }
            })();
            if result.is_err()
                && let Some(sender) = failure.lock().unwrap().take()
            {
                let _ = sender.send(Err("Native page search unavailable".into()));
            }
        })
        .map_err(|_| "Browser unavailable")?;
    let result = tokio::time::timeout(Duration::from_secs(5), receiver).await;
    let registration = cleanup_token.swap(0, std::sync::atomic::Ordering::Relaxed);
    if registration != 0 {
        let _ = window.with_webview(move |platform| unsafe {
            use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_28;
            use windows::core::Interface;
            if let Ok(core) = platform.controller().CoreWebView2()
                && let Ok(core) = core.cast::<ICoreWebView2_28>()
                && let Ok(find) = core.Find()
            {
                let _ = find.remove_ActiveMatchIndexChanged(registration);
            }
        });
    }
    result
        .map_err(|_| "Page search timed out")?
        .map_err(|_| "Page search interrupted")?
}
pub async fn navigation(window: &Webview) -> Result<(bool, bool), String> {
    let history = cdp(window, "Page.getNavigationHistory", json!({})).await?;
    let index = history["currentIndex"].as_u64().unwrap_or(0) as usize;
    let count = history["entries"].as_array().map_or(0, Vec::len);
    Ok((index > 0, index + 1 < count))
}
pub async fn input(window: &Webview, input: BrowserInput) -> Result<(), String> {
    match input {
        BrowserInput::Move { x, y } => {
            cdp(
                window,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y}),
            )
            .await?;
        }
        BrowserInput::Drag { x, y, to_x, to_y } => {
            cdp(
                window,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y}),
            )
            .await?;
            cdp(window,"Input.dispatchMouseEvent",json!({"type":"mousePressed","x":x,"y":y,"button":"left","buttons":1,"clickCount":1})).await?;
            for step in 1..=12 {
                let t = f64::from(step) / 12.0;
                cdp(window,"Input.dispatchMouseEvent",json!({"type":"mouseMoved","x":x+(to_x-x)*t,"y":y+(to_y-y)*t,"button":"left","buttons":1})).await?;
            }
            cdp(window,"Input.dispatchMouseEvent",json!({"type":"mouseReleased","x":to_x,"y":to_y,"button":"left","buttons":0,"clickCount":1})).await?;
        }
        BrowserInput::Click {
            x,
            y,
            button,
            count,
        } => {
            cdp(
                window,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseMoved","x":x,"y":y}),
            )
            .await?;
            for click in 1..=count {
                cdp(
                    window,
                    "Input.dispatchMouseEvent",
                    json!({"type":"mousePressed","x":x,"y":y,"button":button,"clickCount":click}),
                )
                .await?;
                cdp(
                    window,
                    "Input.dispatchMouseEvent",
                    json!({"type":"mouseReleased","x":x,"y":y,"button":button,"clickCount":click}),
                )
                .await?;
            }
        }
        BrowserInput::Scroll {
            x,
            y,
            delta_x,
            delta_y,
        } => {
            cdp(
                window,
                "Input.dispatchMouseEvent",
                json!({"type":"mouseWheel","x":x,"y":y,"deltaX":delta_x,"deltaY":delta_y}),
            )
            .await?;
        }
        BrowserInput::Type { text } => {
            cdp(window, "Input.insertText", json!({"text":text})).await?;
        }
        BrowserInput::Key { key, modifiers } => {
            let bits = modifiers.iter().fold(0, |bits, m| {
                bits | match m.as_str() {
                    "alt" => 1,
                    "control" => 2,
                    "meta" => 4,
                    "shift" => 8,
                    _ => 0,
                }
            });
            let (code, key_code, text) = match key.as_str() {
                "Enter" => ("Enter".into(), 13, "\r".into()),
                "Tab" => ("Tab".into(), 9, String::new()),
                "Escape" => ("Escape".into(), 27, String::new()),
                "Backspace" => ("Backspace".into(), 8, String::new()),
                "Delete" => ("Delete".into(), 46, String::new()),
                "ArrowLeft" => ("ArrowLeft".into(), 37, String::new()),
                "ArrowUp" => ("ArrowUp".into(), 38, String::new()),
                "ArrowRight" => ("ArrowRight".into(), 39, String::new()),
                "ArrowDown" => ("ArrowDown".into(), 40, String::new()),
                "Home" => ("Home".into(), 36, String::new()),
                "End" => ("End".into(), 35, String::new()),
                "PageUp" => ("PageUp".into(), 33, String::new()),
                "PageDown" => ("PageDown".into(), 34, String::new()),
                "Space" => ("Space".into(), 32, " ".into()),
                "F3" => ("F3".into(), 114, String::new()),
                "F5" => ("F5".into(), 116, String::new()),
                "+" | "=" => ("Equal".into(), 187, key.clone()),
                "-" => ("Minus".into(), 189, key.clone()),
                value if value.len() == 1 && value.as_bytes()[0].is_ascii_alphanumeric() => (
                    format!(
                        "{}{}",
                        if value.as_bytes()[0].is_ascii_digit() {
                            "Digit"
                        } else {
                            "Key"
                        },
                        value.to_ascii_uppercase()
                    ),
                    i32::from(value.as_bytes()[0].to_ascii_uppercase()),
                    value.into(),
                ),
                _ => return Err("Unsupported browser key".into()),
            };
            let mut event = json!({"type":"keyDown","key":if key=="Space"{" "}else{&key},"code":code,"windowsVirtualKeyCode":key_code,"nativeVirtualKeyCode":key_code,"modifiers":bits});
            if bits == 0 {
                event["text"] = json!(text);
            }
            cdp(window, "Input.dispatchKeyEvent", event.clone()).await?;
            event["type"] = json!("keyUp");
            event.as_object_mut().unwrap().remove("text");
            cdp(window, "Input.dispatchKeyEvent", event).await?;
        }
        BrowserInput::Reload => {
            cdp(window, "Page.reload", json!({})).await?;
        }
        BrowserInput::Back | BrowserInput::Forward => {
            let history = cdp(window, "Page.getNavigationHistory", json!({})).await?;
            let index = history["currentIndex"]
                .as_i64()
                .ok_or("Browser history unavailable")?
                + if matches!(input, BrowserInput::Back) {
                    -1
                } else {
                    1
                };
            let target = usize::try_from(index)
                .ok()
                .and_then(|index| history["entries"].get(index))
                .and_then(|entry| entry["id"].as_i64())
                .ok_or("No browser history entry in that direction")?;
            cdp(
                window,
                "Page.navigateToHistoryEntry",
                json!({"entryId":target}),
            )
            .await?;
        }
        BrowserInput::Navigate { .. } => {
            return Err("Navigation is dispatched by the page host".into());
        }
    }
    Ok(())
}

pub async fn configure(view: &Webview, locale: &str) -> Result<(), String> {
    let version = cdp(view, "Browser.getVersion", json!({})).await?;
    let user_agent = version["userAgent"]
        .as_str()
        .ok_or("Browser language unavailable")?;
    cdp(view,"Network.setUserAgentOverride",json!({"userAgent":user_agent,"acceptLanguage":format!("{locale},{}",locale.split('-').next().unwrap_or("en"))})).await?;
    cdp(
        view,
        "Emulation.setLocaleOverride",
        json!({"locale":locale}),
    )
    .await?;
    Ok(())
}
