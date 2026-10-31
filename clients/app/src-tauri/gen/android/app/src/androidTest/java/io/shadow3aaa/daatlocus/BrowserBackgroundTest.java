package io.shadow3aaa.daatlocus;

import android.app.Dialog;
import android.app.Instrumentation;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.SystemClock;
import android.util.Base64;
import android.view.MotionEvent;
import android.view.inputmethod.EditorInfo;
import android.webkit.WebView;
import org.json.JSONArray;
import org.json.JSONObject;
import org.json.JSONTokener;
import java.io.BufferedReader;
import java.io.InputStreamReader;
import java.lang.ref.WeakReference;
import java.lang.reflect.Field;
import java.net.InetAddress;
import java.net.ServerSocket;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.HashMap;
import java.util.Map;
import java.util.UUID;
import java.util.concurrent.Callable;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.TimeUnit;

/** Real release WebViews. The test APK must not borrow Kotlin classes removed by app R8. */
final class BrowserBackgroundTest {
    private final Instrumentation instrumentation;
    private final MainActivity owner;
    private final String profile = id() + id(), a = id(), b = id();
    private final Map<String, JSONObject> frames = new HashMap<>();
    private BrowserBackgroundTest(Instrumentation instrumentation, MainActivity owner) {
        this.instrumentation = instrumentation;
        this.owner = owner;
    }
    private static String id() { return UUID.randomUUID().toString().replace("-", ""); }
    private static void require(boolean accepted, String reason) {
        if (!accepted) throw new AssertionError(reason);
    }
    private <T> T ui(Callable<T> block) throws Exception {
        CompletableFuture<T> result = new CompletableFuture<>();
        instrumentation.runOnMainSync(() -> {
            try { result.complete(block.call()); }
            catch (Throwable error) { result.completeExceptionally(error); }
        });
        return result.get(10, TimeUnit.SECONDS);
    }
    private Object evaluate(WebView view, String script) throws Exception {
        CompletableFuture<Object> result = new CompletableFuture<>();
        ui(() -> { view.evaluateJavascript(script, value -> {
            try { result.complete(new JSONTokener(value).nextValue()); }
            catch (Throwable error) { result.completeExceptionally(error); }
        }); return null; });
        return result.get(10, TimeUnit.SECONDS);
    }
    private static Object hostField(String name) throws Exception {
        Field field = WebHost.class.getDeclaredField(name);
        field.setAccessible(true);
        return field.get(null);
    }
    private WebView shell() throws Exception {
        return (WebView) ((WeakReference<?>) hostField("clientView")).get();
    }
    // Inspect by field type, not R8's private Page member names. @Keep WebHost
    // is the same existing JNI boundary used by the product, without a test bridge.
    private <T> T pageField(String tab, Class<T> type) throws Exception {
        Object page = ((Map<?, ?>) hostField("pages")).get(profile + "-" + tab);
        if (page == null) throw new AssertionError("Missing native page " + tab);
        for (Field field : page.getClass().getDeclaredFields()) {
            if (type.isAssignableFrom(field.getType())) {
                field.setAccessible(true);
                return type.cast(field.get(page));
            }
        }
        throw new AssertionError("Missing native page field " + type.getName());
    }
    private WebView view(String tab) throws Exception { return ui(() -> pageField(tab, WebView.class)); }
    private JSONObject control(String action, String tab) throws Exception {
        JSONObject command = new JSONObject().put("profile", profile).put("action", action);
        if (tab != null) command.put("tab", tab);
        // The async product entry point asks for the selected page's color.
        command.put("background_script", "(()=>{const c=getComputedStyle(document.body).backgroundColor.match(/[0-9]+/g);return c?'#'+c.slice(0,3).map(n=>Number(n).toString(16).padStart(2,'0')).join(''):null})()");
        JSONObject reply = new JSONObject(WebHost.browserControl(command.toString()));
        require(reply.isNull("error"), "Native browser control: " + reply);
        return reply;
    }
    private JSONObject request(JSONObject command) throws Exception {
        JSONObject envelope = new JSONObject().put("profile", profile).put("id", id())
            .put("operation", new JSONObject().put("operation", "browser").put("command", command));
        JSONObject reply = new JSONObject(WebHost.request(envelope.toString(), "",
            "(() => ({width:innerWidth,height:innerHeight}))()", "(_event)=>({})"));
        require(reply.isNull("error"), command + " => " + reply);
        JSONObject browser = reply.optJSONObject("browser");
        JSONObject observation = browser == null ? null : browser.optJSONObject("observation");
        if (observation != null) frames.put(command.getString("tab"), observation);
        return reply;
    }
    private void input(String tab, String kind, JSONObject values) throws Exception {
        values.put("kind", kind);
        request(new JSONObject().put("action", "input").put("tab", tab)
            .put("state_id", frames.get(tab).getString("state_id")).put("input", values));
    }
    private void click(String tab) throws Exception {
        JSONObject frame = frames.get(tab);
        input(tab, "click", new JSONObject().put("x", frame.getInt("width") * .2)
            .put("y", frame.getInt("height") * .11).put("button", "left").put("count", 1));
    }
    private void unchanged(WebView ownerPage) throws Exception {
        require(ui(owner::hasWindowFocus), "Background operation stole Owner window focus");
        require(control("list", null).getString("selected").equals(a), "Agent changed Owner selection");
        require(ui(() -> owner.getCurrentFocus() == ownerPage), "Agent changed Owner keyboard focus");
    }
    private void ownerType(String text) throws Exception {
        ui(() -> {
            require(owner.getCurrentFocus() instanceof WebView, "Owner focus left its WebView");
            WebView current = (WebView) owner.getCurrentFocus();
            require(current.onCreateInputConnection(new EditorInfo()).commitText(text, 1), "Owner input rejected");
            return null;
        });
    }
    static String run(Instrumentation instrumentation, MainActivity owner) throws Exception {
        return new BrowserBackgroundTest(instrumentation, owner).run();
    }
    private String run() throws Exception {
        ServerSocket server = new ServerSocket(0, 8, InetAddress.getByName("127.0.0.1"));
        Thread serving = new Thread(() -> {
            while (!server.isClosed()) {
                try (Socket socket = server.accept()) {
                    BufferedReader input = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.UTF_8));
                    String first = input.readLine(), line;
                    while ((line = input.readLine()) != null && !line.isEmpty()) {}
                    String color = first != null && first.contains("/background") ? "rgb(0,200,0)" : "white";
                    byte[] body = ("<!doctype html><style>html,body{margin:0;background:" + color + ";height:2000px}input{position:fixed;left:10%;top:10%;width:60%;height:40px}</style><input><script>window.events=[];for(const kind of ['pointerdown','pointerup','pointermove','mousedown','mouseup','click','auxclick','contextmenu'])document.addEventListener(kind,e=>{events.push({kind,trusted:e.isTrusted,x:e.clientX,y:e.clientY,button:e.button,buttons:e.buttons});if(kind==='contextmenu'||kind==='auxclick')e.preventDefault();});</script>").getBytes(StandardCharsets.UTF_8);
                    socket.getOutputStream().write(("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: " + body.length + "\r\nConnection: close\r\n\r\n").getBytes(StandardCharsets.UTF_8));
                    socket.getOutputStream().write(body);
                } catch (Exception error) { if (!server.isClosed()) throw new RuntimeException(error); }
            }
        }, "browser-release-fixture");
        serving.setDaemon(true);
        serving.start();
        try {
            long deadline = SystemClock.elapsedRealtime() + 30000;
            while (ui(this::shell) == null) {
                require(SystemClock.elapsedRealtime() < deadline, "Native shell did not start");
                SystemClock.sleep(100);
            }
            request(new JSONObject().put("action", "open").put("tab", a).put("url", "http://127.0.0.1:" + server.getLocalPort() + "/owner"));
            require(control("list", null).isNull("selected"), "Agent opening selected its tab");
            control("select", a);
            JSONObject bounds = ui(() -> new JSONObject().put("x", 0).put("y", 0)
                .put("width", shell().getWidth()).put("height", shell().getHeight()).put("scale", 1));
            WebHost.browserControl(new JSONObject().put("profile", profile).put("action", "mount").put("bounds", bounds).toString());
            instrumentation.waitForIdleSync();
            WebView ownerPage = view(a);
            ui(() -> {
                ownerPage.requestFocus();
                long now = SystemClock.uptimeMillis();
                for (int kind : new int[]{MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP}) {
                    MotionEvent event = MotionEvent.obtain(now, now, kind, ownerPage.getWidth() * .2f, ownerPage.getHeight() * .11f, 0);
                    try { ownerPage.dispatchTouchEvent(event); } finally { event.recycle(); }
                }
                return null;
            });
            require(ui(owner::hasWindowFocus), "Owner Activity was not focused");
            request(new JSONObject().put("action", "open").put("tab", b).put("url", "http://127.0.0.1:" + server.getLocalPort() + "/background"));
            unchanged(ownerPage);
            require(ui(() -> !pageField(b, Dialog.class).getWindow().getDecorView().hasWindowFocus()), "Background native window became focused");
            click(b); unchanged(ownerPage);
            require(evaluate(view(b), "document.activeElement.tagName").equals("INPUT"), "Background click did not focus its DOM field");
            ownerType("U"); input(b, "type", new JSONObject().put("text", "Agent")); unchanged(ownerPage); ownerType("V");
            input(b, "key", new JSONObject().put("key", "L").put("modifiers", new JSONArray().put("control")));
            unchanged(ownerPage); ownerType("W");
            Object ownerValue = evaluate(ownerPage, "document.querySelector('input').value");
            require(ownerValue.equals("UVW"), "Owner fixture input changed: " + ownerValue);
            require(evaluate(view(b), "document.querySelector('input').value").equals("Agent"), "Agent input changed");
            byte[] png = Base64.decode(frames.get(b).getString("png_base64"), Base64.NO_WRAP);
            Bitmap bitmap = BitmapFactory.decodeByteArray(png, 0, png.length);
            try { require(bitmap.getPixel(0, 0) == android.graphics.Color.rgb(0, 200, 0), "Background snapshot was not the target page"); }
            finally { bitmap.recycle(); }
            String[] buttons = {"right", "middle"}; int[] codes = {2, 1};
            for (int i = 0; i < buttons.length; i++) {
                JSONObject frame = frames.get(b);
                input(b, "click", new JSONObject().put("x", frame.getInt("width") * .2).put("y", frame.getInt("height") * .11).put("button", buttons[i]).put("count", 1));
                unchanged(ownerPage);
                require(Boolean.TRUE.equals(evaluate(view(b), "events.some(e=>e.kind==='mousedown'&&e.button===" + codes[i] + "&&e.trusted)")), "Native button not delivered");
            }
            JSONObject frame = frames.get(b);
            input(b, "drag", new JSONObject().put("x", frame.getInt("width") * .2).put("y", frame.getInt("height") * .5).put("to_x", frame.getInt("width") * .4).put("to_y", frame.getInt("height") * .5));
            unchanged(ownerPage);
            require(Boolean.TRUE.equals(evaluate(view(b), "events.some(e=>e.kind==='pointermove'&&e.buttons===1&&e.trusted)")), "Native drag not delivered");
            frame = frames.get(b);
            input(b, "scroll", new JSONObject().put("x", frame.getInt("width") * .5).put("y", frame.getInt("height") * .5).put("delta_x", 0).put("delta_y", 96));
            unchanged(ownerPage);
            require(((Number) evaluate(view(b), "scrollY")).doubleValue() > 0, "Background native scroll did not move the page");
            control("select", b);
            require(evaluate(view(b), "document.querySelector('input').value").equals("Agent"), "Owner switch lost background input");
            click(b); input(b, "type", new JSONObject().put("text", "X"));
            require(evaluate(view(b), "document.querySelector('input').value").equals("AgentX"), "Selected tab input failed");
            control("select", a);
            request(new JSONObject().put("action", "observe").put("tab", b));
            require(control("list", null).getString("selected").equals(a), "Background observation selected its tab");
            request(new JSONObject().put("action", "close").put("tab", b));
            require(control("list", null).getString("selected").equals(a), "Background close selected its tab");
            return "background selection, keyboard isolation, live snapshots and Owner switching passed";
        } finally {
            for (String tab : new String[]{a, b}) {
                try { request(new JSONObject().put("action", "close").put("tab", tab)); }
                catch (Exception | AssertionError ignored) { /* The fixture may already have closed this page. */ }
            }
            server.close(); serving.join(2000);
        }
    }
}
