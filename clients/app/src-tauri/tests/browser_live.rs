//! Opt-in native WebView2 integration. Uses only an isolated local fixture.
#![cfg(all(any(windows, target_os = "macos"), feature = "web-contract"))]

use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

struct Fixture {
    address: String,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}
impl Fixture {
    fn start(collector: Option<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let quitting = stop.clone();
        let worker = thread::spawn(move || {
            while !quitting.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = [0u8; 4096];
                let _ = stream.read(&mut request);
                let request = String::from_utf8_lossy(&request);
                if request.starts_with("GET /shell.css ") {
                    let body = include_str!("../../../../webui/src/index.css");
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body.as_bytes());
                    continue;
                }
                if request.starts_with("GET /collector.js ") {
                    let body = collector.as_ref().expect("Collector fixture unavailable");
                    let header = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(header.as_bytes());
                    let _ = stream.write_all(body);
                    continue;
                }
                let body = if request.starts_with("GET /shell ") {
                    r#"<!doctype html><html data-native-browser><meta charset="utf-8"><link rel="stylesheet" href="/shell.css"><style>html,body{margin:0;background:transparent}header{height:100px;box-sizing:border-box}button{position:fixed;left:100px;top:200px;width:200px;height:200px;border:0;background:rgba(255,0,0,.5);box-shadow:0 0 20px #0008}.dl-browser-progress{position:fixed;left:0;right:0;top:138px}</style><header class="dl-browser-tabs">Shell</header><div class="dl-browser-progress"></div><div id="viewport" style="position:fixed;left:80px;top:140px;right:0;bottom:0"><div style="position:absolute;left:350px;top:60px;width:100px;height:100px;background:rgba(0,0,255,.5)" onclick="window.tips=(window.tips||0)+1"></div></div><script type="module">import {browserForeground} from "/collector.js";window.DLComposition={browserForeground};</script><button onclick="window.clicks=(window.clicks||0)+1">Overlay</button>"#
                } else if request.starts_with("GET /composition-page ") {
                    r#"<!doctype html><meta charset="utf-8"><style>html,body{margin:0;width:100%;height:100%;background:rgb(0,200,0)}</style><input aria-label="Native field" style="position:fixed;left:600px;top:100px;width:200px;height:40px"><script>document.onclick=()=>window.nativeControlResult=String(Number(window.nativeControlResult||0)+1)</script>"#
                } else if request.starts_with("GET /background ") {
                    r#"<!doctype html><meta charset="utf-8"><title>Background fixture</title>
<style>html,body{margin:0;background:rgb(0,200,0);height:2000px}input{position:fixed;left:10%;top:10%;width:60%;height:40px}canvas{position:fixed;left:55%;top:30%;width:40%;height:40%}</style>
<input aria-label="Background input"><canvas></canvas>
<script>
// A canvas UI tracks its pointer independently, as native/Wasm UI toolkits do.
// Wheel coordinates alone do not update that application's pointer state.
const canvas=document.querySelector('canvas');
const state=window.nativeControlResult={x:0,y:0,wheels:0,trusted:false};
let pointer={x:0,y:0};
document.addEventListener('mousemove',event=>{pointer={x:event.clientX,y:event.clientY}});
canvas.addEventListener('wheel',event=>{
  event.preventDefault();state.wheels++;state.trusted=event.isTrusted;
  const rect=canvas.getBoundingClientRect();
  if(pointer.x>=rect.left&&pointer.x<rect.right&&pointer.y>=rect.top&&pointer.y<rect.bottom){
    state.x+=event.deltaX;state.y+=event.deltaY;
  }
  const context=canvas.getContext('2d');context.clearRect(0,0,canvas.width,canvas.height);
  context.fillText(`${state.x},${state.y}`,20,30);
},{passive:false});
</script>"#
                } else {
                    r#"<!doctype html><meta charset="utf-8"><title>Live browser fixture</title>
<style>body{height:2000px}input{position:fixed;left:10%;top:10%;width:60%;height:40px}</style>
<input aria-label="Fixture input"><button id="tick">0</button>
<script>setInterval(()=>document.querySelector('#tick').textContent=String(Date.now()),16)</script>"#
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            address,
            stop,
            worker: Some(worker),
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.worker.take().unwrap().join().unwrap();
    }
}

#[cfg(windows)]
#[test]
#[ignore = "requires Windows WebView2; run with --features web-contract --test browser_live -- --ignored"]
fn live_inputs_survive_mutations_owner_actions_and_resize_until_the_tab_closes() {
    let fixture = Fixture::start(None);
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("result.json");
    let tab = "a".repeat(32);
    let input = |value: Value| json!({"action":"input","tab":tab,"state_id":"first","input":value});
    let manifest = json!({
        "output":output,"hidden_browser":true,"relative_points":true,"ui_review":true,
        "panel_steps":{"0":{"x":80,"y":140,"width":1200,"height":860,"scale":1}},
        "owner_select":{"1":tab},
        "delays":{"2":1200},
        "owner_steps":{"3":{"kind":"type","text":"U"}},
        "resize_steps":{"4":[[900,700],[1100,820]]},
        "browser_steps":[
            {"action":"open","tab":tab,"url":fixture.address},
            input(json!({"kind":"click","x":0.2,"y":0.13,"button":"left","count":1})),
            input(json!({"kind":"type","text":"A"})),
            input(json!({"kind":"type","text":"B"})),
            {"action":"observe","tab":tab},
            input(json!({"kind":"type","text":"C"})),
            input(json!({"kind":"scroll","x":0.5,"y":0.5,"delta_x":0,"delta_y":80})),
            input(json!({"kind":"type","text":"D"})),
            {"action":"input","tab":tab,"state_id":"0".repeat(32),"input":{"kind":"type","text":"X"}},
            {"action":"close","tab":tab},
            input(json!({"kind":"type","text":"E"}))
        ]
    });
    let path = root.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_daat-locus-app"))
        .args(["--web-probe"])
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Native browser integration timed out");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(
        status.success(),
        "Native probe failed: {:?}",
        std::fs::read_to_string(&output)
    );
    let steps: Vec<Value> = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(steps.len(), 11);
    for (index, step) in steps.iter().take(10).enumerate() {
        assert!(
            step["reply"]["error"].is_null(),
            "step {index}: {}",
            step["reply"]
        );
    }
    let fields = steps[8]["ui"]["inputs"].as_array().unwrap();
    assert!(
        fields
            .iter()
            .any(|field| field["label"] == "Fixture input" && field["value"] == "AUBCDX"),
        "{fields:?}"
    );
    assert!(
        steps[10]["reply"]["error"]
            .as_str()
            .unwrap()
            .contains("closed")
    );
    cleanup(root);
}

#[cfg(windows)]
#[test]
#[ignore = "requires a visible Windows desktop and WebView2; run opt-in native integration"]
fn transparent_shell_composes_above_the_page_and_routes_native_clicks() {
    let collector = Command::new("bun")
        .args([
            "build",
            "webui/src/lib/browser-composition.ts",
            "--target=browser",
            "--format=esm",
        ])
        .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.."))
        .output()
        .expect("Bun is required for the production DOM collector integration");
    assert!(collector.status.success(), "{:?}", collector.stderr);
    let fixture = Fixture::start(Some(collector.stdout));
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("composition.json");
    let tab = "b".repeat(32);
    let manifest = json!({"output":output,"shell_url":format!("{}shell",fixture.address),"relative_points":true,"ui_review":true,
        "panel_steps":{"0":{"x":80,"y":140,"width":1200,"height":860,"scale":1}},
        "owner_select":{"1":tab},
        "owner_clicks":{"1":[[150,250],[450,250],[500,300]],"2":[[150,250],[450,250],[500,300]],"3":[[700,260]]},
        "owner_text":{"3":"DL中文"},
        "resize_steps":{"1":[[900,700],[1100,820]]},
        "browser_steps":[{"action":"open","tab":tab,"url":format!("{}composition-page",fixture.address)},{"action":"observe","tab":tab},{"action":"observe","tab":tab},{"action":"observe","tab":tab}]
    });
    let path = root.path().join("manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_daat-locus-app"))
        .arg("--web-probe")
        .arg(path)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Composition integration timed out");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{:?}", std::fs::read_to_string(&output));
    let steps: Vec<Value> = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    for (index, expected) in [(1, 1), (2, 2)] {
        let step = &steps[index];
        assert_eq!(
            step["shell"]["clicks"], expected,
            "shell native input: {}",
            step["shell"]
        );
        assert_eq!(
            step["native_control"]["status"],
            expected.to_string(),
            "page native input: {}",
            step["native_control"]
        );
        assert_eq!(step["layer"]["composed"], true, "{}", step["layer"]);
        assert_eq!(
            step["layer"]["background"], "#00c800",
            "read actual page paint"
        );
        assert_eq!(
            step["shell"]["tips"], expected,
            "nested foreground native input"
        );
        let pixels = step["layer"]["pixels"].as_array().unwrap();
        for pixel in &pixels[4..6] {
            assert!(
                pixel
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|channel| channel.as_u64().unwrap() > 200),
                "light browser chrome must stay opaque above the native surface: {pixel}"
            );
        }
        assert!(
            pixels[2][1].as_u64().unwrap() < 195,
            "shadow must composite outside the popup bounds: {pixels:?}"
        );
        assert!(
            pixels[3][2].as_u64().unwrap() > 120,
            "nested translucent foreground: {pixels:?}"
        );
        if index == 2 {
            assert_eq!(step["shell"]["width"], 1100, "native shell resize");
        }
        let red = pixels[0][0].as_u64().unwrap();
        let green = pixels[0][1].as_u64().unwrap();
        assert!(
            (120..=135).contains(&red) && (90..=110).contains(&green),
            "alpha overlay was not composited over the webpage: {pixels:?}"
        );
        assert_eq!(
            pixels[1],
            json!([0, 200, 0]),
            "blank shell must reveal the live webpage"
        );
    }
    let resizes = steps[1]["resize_checks"].as_array().unwrap();
    assert_eq!(resizes[0]["page"]["width"], 820);
    assert_eq!(resizes[0]["page"]["height"], 560);
    assert_eq!(resizes[1]["page"]["width"], 1020);
    assert_eq!(resizes[1]["page"]["height"], 680);
    let fields = steps[3]["ui"]["inputs"].as_array().unwrap();
    assert!(
        fields
            .iter()
            .any(|field| field["label"] == "Native field" && field["value"] == "DL中文"),
        "native keyboard focus: {fields:?}"
    );
    cleanup(root);
}

#[test]
#[ignore = "requires native WebView2 or WKWebView; run opt-in native integration"]
fn background_tabs_do_not_change_owner_selection_or_keyboard_focus() {
    use base64::Engine;
    let fixture = Fixture::start(None);
    let root = tempfile::tempdir().unwrap();
    let output = root.path().join("background.json");
    let a = "a".repeat(32);
    let b = "b".repeat(32);
    let input = |tab: &str, value: Value| json!({"action":"input","tab":tab,"state_id":"latest","input":value});
    let shortcut_modifier = if cfg!(target_os = "macos") {
        "meta"
    } else {
        "control"
    };
    let mut manifest = json!({
        "output":output,"hidden_browser":false,"relative_points":true,"inspect_tabs":true,
        "panel_steps":{"0":{"x":0,"y":0,"width":1100,"height":800,"scale":1}},
        "owner_select":{"1":a,"7":b,"8":a},
        "owner_steps":{"2":{"kind":"click","x":200,"y":120,"button":"left","count":1}},
        "browser_steps":[
            {"action":"open","tab":a,"url":fixture.address},
            {"action":"observe","tab":a},
            {"action":"open","tab":b,"url":format!("{}background",fixture.address)},
            input(&b,json!({"kind":"click","x":0.2,"y":0.13,"button":"left","count":1})),
            input(&b,json!({"kind":"type","text":"Agent"})),
            input(&b,json!({"kind":"key","key":"L","modifiers":[shortcut_modifier]})),
            {"action":"observe","tab":b},
            {"action":"observe","tab":a},
            {"action":"observe","tab":b},
            input(&b,json!({"kind":"scroll","x":0.75,"y":0.5,"delta_x":0,"delta_y":80})),
            input(&b,json!({"kind":"scroll","x":0.75,"y":0.5,"delta_x":40,"delta_y":0})),
            input(&b,json!({"kind":"scroll","x":0.1,"y":0.65,"delta_x":0,"delta_y":120})),
            {"action":"close","tab":b}
        ]
    });
    #[cfg(windows)]
    {
        manifest["owner_clicks"] = json!({"2":[[200,120]]});
        manifest["owner_text"] = json!({"3":"U","4":"V","5":"W"});
    }
    #[cfg(target_os = "macos")]
    {
        for (step, text) in [("4", "U"), ("5", "V"), ("6", "W")] {
            manifest["owner_steps"][step] = json!({"kind":"type","text":text});
        }
    }
    let path = root.path().join("background-manifest.json");
    std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_daat-locus-app"))
        .arg("--web-probe")
        .arg(path)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(100);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("Background browser integration timed out");
        }
        thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "{:?}", std::fs::read_to_string(&output));
    let steps: Vec<Value> = serde_json::from_slice(&std::fs::read(output).unwrap()).unwrap();
    assert_eq!(steps.len(), 13);
    for (index, step) in steps.iter().enumerate() {
        assert!(
            step["reply"]["error"].is_null(),
            "step {index}: {}",
            step["reply"]
        );
        let expected = if index == 0 {
            Value::Null
        } else if index == 7 {
            json!(b)
        } else {
            json!(a)
        };
        assert_eq!(
            step["owner_workspace"]["selected"], expected,
            "selection at {index}"
        );
        assert!(
            step["focus_before"]["Ok"].is_object(),
            "native focus probe failed at {index}: {}",
            step["focus_before"]
        );
        #[cfg(windows)]
        assert_eq!(
            step["focus_before"]["Ok"]["available"], true,
            "foreground keyboard information unavailable"
        );
        assert_eq!(
            step["focus_before"], step["focus_after"],
            "native focus at {index}"
        );
    }
    assert_eq!(steps[6]["tab_states"][&a]["value"], "UVW");
    assert_eq!(steps[6]["tab_states"][&b]["value"], "Agent");
    assert_eq!(steps[8]["tab_states"][&b]["value"], "Agent");
    assert_eq!(
        steps[9]["tab_states"][&b]["native_control"]["y"], 80,
        "wheel input must move the canvas UI, not merely return an accepted command"
    );
    assert_eq!(
        steps[10]["tab_states"][&b]["native_control"]["x"], 40,
        "horizontal wheel input must reach the canvas UI"
    );
    assert_eq!(
        steps[10]["tab_states"][&b]["native_control"]["trusted"],
        true
    );
    assert!(
        steps[11]["tab_states"][&b]["scrollY"].as_f64().unwrap() > 0.0,
        "native document scrolling must still work outside the canvas"
    );
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(
            steps[4]["reply"]["browser"]["observation"]["png_base64"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let image = tauri::image::Image::from_bytes(&bytes).unwrap();
    let pixel = &image.rgba()[..4];
    assert!(
        u16::from(pixel[1]) > u16::from(pixel[0]) + 64
            && u16::from(pixel[1]) > u16::from(pixel[2]) + 64
            && pixel[3] == 255,
        "background snapshot must show the green fixture, not the Owner's white page: {pixel:?}"
    );
    cleanup(root);
}

fn cleanup(root: tempfile::TempDir) {
    // WebView2 releases its profile asynchronously after the host exits.
    let directory = root.keep();
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Err(error) = std::fs::remove_dir_all(&directory) {
        assert!(
            Instant::now() < deadline,
            "Isolated profile cleanup failed: {error}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}
