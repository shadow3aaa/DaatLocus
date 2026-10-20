package io.shadow3aaa.daatlocus;

import android.app.Activity;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.os.Bundle;
import android.os.SystemClock;
import org.json.JSONArray;
import org.json.JSONObject;
import java.util.UUID;

/** Opt-in device contract: a disposable account and the Display Probe fixture are required.
 * Exercises the production Owner JNI command and both real privileged transports. */
final class PhoneUseTest {
    private final String actor = "phone_test_" + UUID.randomUUID().toString().replace("-", "");
    private final String session = UUID.randomUUID().toString();
    private final ReleaseStartupTest test;
    private final Activity activity;
    private JSONObject last;
    private byte[] image;
    private boolean mainIme;
    private int mainImeBottom;
    private String phase="initial";
    private PhoneUseTest(ReleaseStartupTest test, Activity activity) { this.test = test; this.activity = activity; }
    static String run(ReleaseStartupTest test, Activity activity, Bundle args) throws Exception {
        return new PhoneUseTest(test, activity).run(args);
    }
    static void require(boolean value, String message) { if (!value) throw new AssertionError(message); }
    private Object js(String script) throws Exception { return test.evaluate(activity, script); }
    private void await(String expression, long timeout) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + timeout;
        while (SystemClock.elapsedRealtime() < deadline) {
            if (Boolean.TRUE.equals(js(expression))) return;
            SystemClock.sleep(100);
        }
        throw new AssertionError("Timed out: " + expression + "; page=" + js("document.body.innerText.slice(0,1600)"));
    }
    private JSONObject call(String op, JSONObject args) throws Exception { return call(actor, op, args, UUID.randomUUID().toString(), "1"); }
    private JSONObject call(String who, String op, JSONObject args, String id, String epoch) throws Exception {
        String request = new JSONObject().put("id", id).put("actor", who).put("session", session)
            .put("epoch", epoch).put("op", op).put("args", args).toString();
        last = new JSONObject(PhoneUseController.request(request)); image = PhoneUseController.image(id);
        return last;
    }
    private JSONObject success(String op, JSONObject args) throws Exception {
        JSONObject reply = call(op, args);
        if (!reply.isNull("failure")) {
            JSONObject windows = call("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100));
            throw new AssertionError(phase+": "+op+" failed: "+reply+"; current windows: "+windows);
        }
        return reply.getJSONObject("payload");
    }
    private void refused(JSONObject reply, String operation) throws Exception {
        require(!reply.isNull("failure") && "not_started".equals(reply.getJSONObject("payload").getString("status")), operation + " was not rejected before dispatch: " + reply);
    }
    private JSONObject observation(JSONObject payload) throws Exception {
        JSONObject observed = payload.getJSONObject("observation");
        Bitmap bitmap = BitmapFactory.decodeByteArray(image, 0, image.length);
        require(bitmap != null && bitmap.getWidth() == observed.getInt("width") && bitmap.getHeight() == observed.getInt("height"), "Missing or incorrectly sized native screenshot");
        require(bitmap.getPixel(bitmap.getWidth()/2, bitmap.getHeight()/2) != bitmap.getPixel(0,0) || image.length > 4096, "Screenshot appears blank");
        bitmap.recycle(); return observed;
    }
    private JSONObject window(JSONObject observed) throws Exception { return new JSONObject().put("window_id", observed.getString("window_id")); }
    private JSONObject state(JSONObject observed) throws Exception { return window(observed).put("state_id", observed.getString("state_id")); }
    private JSONObject displaySettings() throws Exception {
        js("window.__displayStatus=null;window.__TAURI_INTERNALS__.invoke('phone_use_status').then(r=>window.__displayStatus=r);null");
        await("window.__displayStatus!==null",10000);
        return new JSONObject((String)js("JSON.stringify(window.__displayStatus.display)"));
    }
    private void configureDisplay(JSONObject patch) throws Exception {
        js("window.__displaySaved=null;window.__TAURI_INTERNALS__.invoke('phone_use_display_configure',{patch:"+patch+"}).then(r=>window.__displaySaved=r,e=>window.__displaySaved={error:String(e)});null");
        await("window.__displaySaved!==null",10000);
        require(!Boolean.TRUE.equals(js("'error' in window.__displaySaved")),"Display settings were rejected: "+js("JSON.stringify(window.__displaySaved)"));
    }
    private void editDisplay(String key, String value) throws Exception {
        js("(()=>{let e=document.getElementById('dl-phone-use-"+key+"');e.focus();Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,"+JSONObject.quote(value)+");e.dispatchEvent(new Event('input',{bubbles:true}));})();null");
        js("document.getElementById('dl-phone-use-"+key+"').blur();null");
    }
    private void applyDisplay() throws Exception {
        js("document.getElementById('dl-phone-use-width').closest('form').requestSubmit();null");
        await("document.querySelector('[aria-controls=dl-phone-use-advanced]').closest('section').getAttribute('aria-busy')==='false'",10000);
    }
    private java.util.Set<Integer> displayIds() {
        java.util.Set<Integer> ids=new java.util.HashSet<>();
        for (android.view.Display display:activity.getSystemService(android.hardware.display.DisplayManager.class).getDisplays())
            if (display.getName().startsWith("DaatLocus-")) ids.add(display.getDisplayId());
        return ids;
    }
    private JSONObject resizedWindow(int width, int height) throws Exception {
        long until=SystemClock.elapsedRealtime()+5000;
        do {
            JSONArray windows=success("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("windows");
            if (windows.length()==1) {
                JSONObject reply=call("open_window",new JSONObject().put("window_id",windows.getJSONObject(0).getString("window_id")));
                if (reply.isNull("failure")) {
                    JSONObject observed=observation(reply.getJSONObject("payload"));
                    if (observed.getInt("width")==width && observed.getInt("height")==height) return observed;
                }
            }
            SystemClock.sleep(100);
        } while(SystemClock.elapsedRealtime()<until);
        throw new AssertionError("Existing task did not resize to "+width+"x"+height);
    }
    private JSONObject observeAfterInput(JSONObject reply, JSONObject previous) throws Exception {
        if (reply.isNull("failure")) return observation(reply.getJSONObject("payload"));
        require("outcome_unknown".equals(reply.getJSONObject("payload").getString("status")),"Input was rejected before dispatch: "+reply);
        // Android can show an insertion-handle popup after tapping a restored editor.
        // Observe its settled result without replaying the uncertain input.
        long deadline=SystemClock.elapsedRealtime()+6000;
        do {
            JSONObject observed=call("get_window_state",window(previous).put("region",JSONObject.NULL).put("scale",JSONObject.NULL));
            if (observed.isNull("failure")) return observation(observed.getJSONObject("payload"));
            require("not_started".equals(observed.getJSONObject("payload").getString("status")),"Observation had an unexpected effect: "+observed);
            SystemClock.sleep(100);
        } while(SystemClock.elapsedRealtime()<deadline);
        throw new AssertionError("Input result remained obscured: "+reply);
    }
    private void mainUnchanged() throws Exception {
        require(activity.hasWindowFocus(), "Virtual GUI operation stole main-screen window focus");
        final boolean[] ime = new boolean[1];
        final int[] bottom = new int[1];
        test.runOnMainSync(() -> {
            android.view.WindowInsets insets=activity.getWindow().getDecorView().getRootWindowInsets();
            ime[0]=insets.isVisible(android.view.WindowInsets.Type.ime()); bottom[0]=insets.getInsets(android.view.WindowInsets.Type.ime()).bottom;
        });
        require(ime[0] == mainIme, "Virtual GUI operation changed main-screen IME visibility");
        require(bottom[0] == mainImeBottom, "Virtual GUI operation changed main-screen IME geometry");
    }
    private String run(Bundle args) throws Exception {
        String method = args.getString("method");
        require("root".equals(method) || "shizuku".equals(method), "Pass method=root or method=shizuku");
        String name = args.getString("account");
        require(name != null && name.startsWith("phone-test-"), "Pass a dedicated phone-test-* account name");
        refused(new JSONObject(PhoneUseHost.handle(test.getTargetContext(), "{\"op\":\"probe\"}").getJson()), "Unprivileged application UID");
        String password = args.getString("password");
        require(password != null && password.length() >= 12, "Pass the disposable test account password");
        await("!!window.__TAURI_INTERNALS__ && document.readyState==='complete'", 30000);
        await("!!document.querySelector('.dl-access-gate') || !!document.querySelector('.dl-app-shell')",30000);
        js("window.__phoneInfo=null;window.__TAURI_INTERNALS__.invoke('desktop_account',{input:{kind:'info'}}).then(r=>window.__phoneInfo=r);null");
        await("window.__phoneInfo!==null", 10000);
        JSONObject info = new JSONObject((String)js("JSON.stringify(window.__phoneInfo.body)"));
        if (!info.getBoolean("registered")) {
            js("window.__phoneDenied=null;window.__TAURI_INTERNALS__.invoke('phone_use_configure',{enabled:true,method:'"+method+"'}).then(()=>window.__phoneDenied=false,()=>window.__phoneDenied=true);null");
            await("window.__phoneDenied!==null",10000);
            require(Boolean.TRUE.equals(js("window.__phoneDenied")), "Unauthenticated native configuration was accepted");
            js("window.__phoneDisplayDenied=null;window.__TAURI_INTERNALS__.invoke('phone_use_display_configure',{patch:{width:1024}}).then(()=>window.__phoneDisplayDenied=false,()=>window.__phoneDisplayDenied=true);null");
            await("window.__phoneDisplayDenied!==null",10000);
            require(Boolean.TRUE.equals(js("window.__phoneDisplayDenied")),"Unauthenticated display settings were accepted");
            await("!!document.querySelector('.dl-access-gate > button')",10000);
            js("document.querySelector('.dl-access-gate > button').click();null");
            await("!!document.querySelector('#dl-account-confirmation')",5000);
            // Random one-use credentials stay in this test process and the normal OPAQUE form.
            js("(()=>{const set=(id,value)=>{let e=document.getElementById(id);Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,value);e.dispatchEvent(new Event('input',{bubbles:true}));};set('dl-account-name',"+JSONObject.quote(name)+");set('dl-access-credential',"+JSONObject.quote(password)+");set('dl-account-confirmation',"+JSONObject.quote(password)+");})();null");
            js("document.querySelector('.dl-access-gate form').requestSubmit();null");
            await("!document.querySelector('.dl-access-gate')",60000);
        } else {
            require(name.equals(info.optString("name")), "Refusing to use a non-test account");
            if (Boolean.TRUE.equals(js("!!document.querySelector('.dl-access-gate')"))) {
                await("document.getElementById('dl-account-name')?.value==="+JSONObject.quote(name),10000);
                js("(()=>{const set=(id,value)=>{let e=document.getElementById(id);Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(e,value);e.dispatchEvent(new Event('input',{bubbles:true}));};set('dl-account-name',"+JSONObject.quote(name)+");set('dl-access-credential',"+JSONObject.quote(password)+");})();null");
                js("document.querySelector('.dl-access-gate form').requestSubmit();null");
            }
            await("!document.querySelector('.dl-access-gate')",60000);
        }
        if ("true".equals(args.getString("verify_display"))) {
            JSONObject persisted=displaySettings();
            require(persisted.getInt("width")==960 && persisted.getInt("height")==640 && persisted.getInt("density")==200,"Display preference lost after process restart: "+persisted);
            return "Owner display settings persist across process restart";
        }
        js("window.__edgeDisabled=false;window.__TAURI_INTERNALS__.invoke('phone_use_configure',{enabled:false,method:'"+method+"'}).then(()=>window.__edgeDisabled=true);null");
        await("window.__edgeDisabled",10000);
        EdgeToEdgeTest.run(test, activity);
        configureDisplay(new JSONObject().put("width",1280).put("height",720).put("density",160));
        js("history.pushState(null,'','/settings/gui');window.dispatchEvent(new PopStateEvent('popstate'));null");
        await("!!document.querySelector('#dl-phone-use-enabled:not(:disabled)')",10000);
        js("document.querySelector('[aria-controls=dl-phone-use-advanced]').click();null");
        await("!!document.getElementById('dl-phone-use-width')",5000);
        editDisplay("width","0");
        applyDisplay();
        await("document.getElementById('dl-phone-use-width').getAttribute('aria-invalid')==='true'",5000);
        require("0".equals(js("document.getElementById('dl-phone-use-width').value")),"Invalid display draft was discarded");
        require(displaySettings().getInt("width")==1280,"Invalid display setting replaced persisted geometry");
        editDisplay("width","1024"); editDisplay("height","640");
        require(displaySettings().getInt("width")==1280,"Display changed before Owner confirmation");
        applyDisplay();
        long displayDeadline=SystemClock.elapsedRealtime()+10000;
        JSONObject size=displaySettings();
        while ((size.getInt("width")!=1024 || size.getInt("height")!=640) && SystemClock.elapsedRealtime()<displayDeadline) {SystemClock.sleep(100);size=displaySettings();}
        require(size.getInt("width")==1024 && size.getInt("height")==640,"Confirmed UI display edits were lost: "+size);
        js("window.__displayRejected=null;window.__TAURI_INTERNALS__.invoke('phone_use_display_configure',{patch:{width:4096,height:4096}}).then(()=>window.__displayRejected=false,()=>window.__displayRejected=true);null");
        await("window.__displayRejected!==null",10000);
        require(Boolean.TRUE.equals(js("window.__displayRejected")),"Native display pixel limit was not enforced");
        require(displaySettings().getInt("width")==1024 && displaySettings().getInt("height")==640,"Rejected native patch changed existing preferences");
        js("document.querySelector('#dl-phone-use-enabled').click();null");
        await("!!document.querySelector('#dl-phone-use-method')",10000);
        js("Array.from(document.querySelectorAll('#dl-phone-use-method [data-slot=toggle-group-item]')).find(e=>e.textContent.trim()==="+JSONObject.quote(method.equals("root")?"Root":"Shizuku")+").click();null");
        await("!document.querySelector('#dl-phone-use-method') && document.querySelector('#dl-phone-use-enabled')?.getAttribute('aria-checked')==='true'",75000);
        js("window.__phoneConfigured=null;window.__TAURI_INTERNALS__.invoke('phone_use_status').then(r=>window.__phoneConfigured=r);null");
        await("window.__phoneConfigured!==null",10000);
        JSONObject configured = new JSONObject((String)js("JSON.stringify(window.__phoneConfigured)"));
        require("ready".equals(configured.optString("state")), "Real authorization/probe failed: " + configured);
        require(configured.getInt("uid") == (method.equals("root") ? 0 : 2000), "Unexpected privileged identity");
        js("history.pushState(null,'','/');window.dispatchEvent(new PopStateEvent('popstate'));null");
        require(activity.hasWindowFocus(), "Main-screen test window must own focus before GUI operations");
        final boolean[] ime = new boolean[1];
        test.runOnMainSync(() -> {
            android.view.WindowInsets insets=activity.getWindow().getDecorView().getRootWindowInsets();
            ime[0]=insets.isVisible(android.view.WindowInsets.Type.ime()); mainImeBottom=insets.getInsets(android.view.WindowInsets.Type.ime()).bottom;
        });
        mainIme = ime[0];
        JSONArray apps = success("list_apps", new JSONObject().put("query","PhoneUse Test Fixture").put("app_id",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("apps");
        require(apps.length()==1, "Install the phone-use-fixture APK");
        android.util.Log.i("PhoneUseTest","Native launch");
        JSONObject observed = observation(success("launch_app",new JSONObject().put("app_id",apps.getJSONObject(0).getString("app_id"))));
        require(observed.getInt("width")==1024 && observed.getInt("height")==640 && observed.getJSONObject("display").getInt("density")==160,"New display did not use Owner settings: "+observed);
        mainUnchanged();
        java.nio.file.Files.write(new java.io.File(test.getTargetContext().getCacheDir(), "phone-use-test.png").toPath(),image);
        // Fixture's documented landscape layout places its editable control here.
        JSONObject tapped = observation(success("click",state(observed).put("x",640).put("y",96).put("button","left").put("click_count",1)));
        mainUnchanged();
        require(!tapped.isNull("input_target"), "Fixture editable receiver was not selected");
        refused(call("set_text",state(observed).put("text","stale")), "Stale state");
        String text = "虚拟屏幕测试 Android 🚀 " + method;
        observed = observation(success("set_text",state(tapped).put("text",text)));
        require(text.equals(observed.getJSONObject("input_target").getString("text")), "Unicode replacement did not reach the actual editable control");
        mainUnchanged();
        observed = observation(success("press_key",state(observed).put("key","Backspace")));
        require(text.substring(0,text.length()-1).equals(observed.getJSONObject("input_target").getString("text")),"Key input did not reach the virtual display");
        mainUnchanged();
        phase="live display resize";
        java.util.Set<Integer> existingDisplays=displayIds();
        require(existingDisplays.size()==1,"Expected one active Actor display");
        js("history.pushState(null,'','/settings/gui');window.dispatchEvent(new PopStateEvent('popstate'));null");
        await("!!document.querySelector('[aria-controls=dl-phone-use-advanced]')",5000);
        if (!Boolean.TRUE.equals(js("document.querySelector('[aria-controls=dl-phone-use-advanced]').getAttribute('aria-expanded')==='true'")))
            js("document.querySelector('[aria-controls=dl-phone-use-advanced]').click();null");
        await("!!document.getElementById('dl-phone-use-width')",5000);
        editDisplay("width","1440"); editDisplay("height","900"); editDisplay("density","240");
        require(displaySettings().getInt("width")==1024,"Editing a draft resized an active display");
        applyDisplay();
        JSONObject applied=displaySettings();
        require(applied.getInt("width")==1440 && applied.getInt("height")==900 && applied.getInt("density")==240,"Confirmed live resize was not saved: "+applied);
        require(existingDisplays.equals(displayIds()),"Applying parameters created or replaced a display");
        refused(call("get_window_state",window(observed).put("region",JSONObject.NULL).put("scale",JSONObject.NULL)),"Pre-resize screenshot reference");
        observed=resizedWindow(1440,900);
        observed=observeAfterInput(call("click",state(observed).put("x",640).put("y",144).put("button","left").put("click_count",1)),observed);
        require(observed.getJSONObject("display").getInt("density")==240 && text.substring(0,text.length()-1).equals(observed.getJSONObject("input_target").getString("text")),"Live resize lost task contents or DPI");
        js("history.pushState(null,'','/');window.dispatchEvent(new PopStateEvent('popstate'));null");
        mainUnchanged();
        JSONObject sibling = call(actor+"_other","launch_app",new JSONObject().put("app_id",apps.getJSONObject(0).getString("app_id")),UUID.randomUUID().toString(),"1");
        require(sibling.isNull("failure"),"Second Actor could not launch its own task: "+sibling);
        JSONObject siblingWindow = observation(sibling.getJSONObject("payload"));
        require(siblingWindow.getInt("width")==1440 && siblingWindow.getInt("height")==900 && siblingWindow.getJSONObject("display").getInt("density")==240,"New Actor display ignored updated resolution/DPI");
        JSONObject closedSibling = call(actor+"_other","close_window",state(siblingWindow),UUID.randomUUID().toString(),"1");
        require(closedSibling.isNull("failure"),"Second Actor task did not close");
        observed = observation(success("get_window_state",window(observed).put("region",JSONObject.NULL).put("scale",JSONObject.NULL)));
        require(observed.getInt("width")==1440 && observed.getInt("height")==900 && observed.getJSONObject("display").getInt("density")==240,"Updated display parameters did not remain active");
        require(text.substring(0,text.length()-1).equals(observed.getJSONObject("input_target").getString("text")),"Closing another Actor task destroyed this Actor's app state");
        refused(call(actor+"_other","get_window_state",window(observed).put("region",JSONObject.NULL).put("scale",JSONObject.NULL),UUID.randomUUID().toString(),"1"),"Cross-Actor window");
        String cancelled = UUID.randomUUID().toString(); PhoneUseController.cancel(cancelled);
        refused(call(actor,"set_text",state(observed).put("text","cancelled"),cancelled,"1"),"Cancelled operation");
        refused(call(actor,"set_text",state(observed).put("text","old epoch"),UUID.randomUUID().toString(),"0"),"Stale epoch");
        JSONObject crop = observation(success("get_window_state",window(observed).put("region",new JSONObject().put("x",100).put("y",100).put("width",300).put("height",200)).put("scale",2)));
        require(crop.getInt("width")==600 && crop.getInt("height")==400,"Crop/scale contract failed");
        success("release_window",window(crop));
        refused(call("get_window_state",window(crop).put("region",JSONObject.NULL).put("scale",JSONObject.NULL)),"Released window");
        JSONArray windows = success("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("windows");
        require(windows.length()==1 && !windows.getJSONObject(0).getString("window_id").equals(crop.getString("window_id")),"Released task not preserved with a new reference");
        observed = observation(success("open_window",new JSONObject().put("window_id",windows.getJSONObject(0).getString("window_id"))));
        success("close_window",state(observed));
        require(success("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("windows").length()==0,"Closed task remains discoverable");
        phase="detach/reset";
        configureDisplay(new JSONObject().put("width",1280).put("height",720).put("density",160));
        observed = observation(success("launch_app",new JSONObject().put("app_id",apps.getJSONObject(0).getString("app_id"))));
        success("detach",new JSONObject());
        require(success("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("windows").length()==1,"Ordinary detach destroyed the task");
        PhoneUseController.reset();
        require(success("list_windows",new JSONObject().put("app_id",JSONObject.NULL).put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)).getJSONArray("windows").length()==0,"Account reset left Actor displays alive");
        mainUnchanged();
        android.util.Log.i("PhoneUseTest","Actor integration");
        integration();
        js("window.__phoneOff=null;window.__TAURI_INTERNALS__.invoke('phone_use_configure',{enabled:false,method:'"+method+"'}).then(r=>window.__phoneOff=r);null");
        await("window.__phoneOff!==null",15000);
        refused(call("list_apps",new JSONObject().put("query",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100)),"Disabled backend");
        configureDisplay(new JSONObject().put("width",960).put("height",640).put("density",200));
        return "Edge-to-edge portrait/landscape, drawer/dialog and IME transitions; Owner gate; "+method+" UID; native PNG; Unicode; key input; unchanged main focus/IME; stale state/epoch; cancellation; Actor isolation and task preservation; crop; release/reacquire; close/relaunch; detach/reset; Actor/Rust/JNI/resource integration; disabled gate";
    }
    private JSONObject http(String path,String method,JSONObject body) throws Exception {
        js("window.__phoneHTTP=null;"+ReleaseStartupTest.nativeFetch()+"("+JSONObject.quote(path)+",{method:"+JSONObject.quote(method)+",headers:{'Content-Type':'application/json','Idempotency-Key':'"+UUID.randomUUID()+"'},body:"+(body==null?"undefined":JSONObject.quote(body.toString()))+"}).then(async r=>window.__phoneHTTP={status:r.status,body:await r.text()}).catch(e=>window.__phoneHTTP={status:0,body:String(e)});null");
        await("window.__phoneHTTP!==null",30000);
        JSONObject reply=new JSONObject((String)js("JSON.stringify(window.__phoneHTTP)"));
        require(reply.getInt("status")>=200 && reply.getInt("status")<300,"Owner test HTTP failed: "+path+" "+reply);
        return reply;
    }
    private void integration() throws Exception {
        String stage=new JSONObject(http("/v1/stage","GET",null).getString("body")).getString("stage_id");
        try(PhoneUseProvider provider=new PhoneUseProvider(stage)) {
            JSONObject config=new JSONObject().put("providers",new JSONArray().put(new JSONObject().put("kind","openai_compatible").put("name","phone-test").put("api_key","disposable-fixture-key").put("base_url","http://127.0.0.1:"+provider.server.getLocalPort()+"/v1")))
                .put("models",new JSONArray().put(new JSONObject().put("name","phone-test").put("provider_name","phone-test").put("model_id","phone-test").put("supports_vision",true).put("context_window_tokens",128000).put("max_completion_tokens",4096).put("request_timeout_secs",20)))
                .put("main_model","phone-test").put("approval_model","phone-test");
            http("/v1/config","PUT",config);
            long readyDeadline=SystemClock.elapsedRealtime()+45000;
            boolean ready=false;
            JSONObject status=new JSONObject();
            while(SystemClock.elapsedRealtime()<readyDeadline) {
                status=new JSONObject(http("/v1/stage/status","GET",null).getString("body"));
                JSONObject core=status.optJSONObject("core");
                if(core!=null && core.optBoolean("ready")) { ready=true;break; }
                SystemClock.sleep(200);
            }
            require(ready,"Account Core was not ready for the integration fixture: "+status.optJSONObject("core"));
            http("/v1/actors/core/messages","POST",new JSONObject().put("content","Run the local PhoneUse integration fixture").put("attachments",new JSONArray()));
            long deadline=SystemClock.elapsedRealtime()+60000;
            while(!provider.done && provider.failure==null && SystemClock.elapsedRealtime()<deadline) SystemClock.sleep(100);
            require(provider.done && provider.failure==null,"Actor/Rust/JNI integration did not complete: "+provider.failure);
            js("window.__phoneResource=null;"+ReleaseStartupTest.nativeFetch()+"("+JSONObject.quote(provider.screenshot)+").then(async r=>{let b=new Uint8Array(await r.arrayBuffer());window.__phoneResource=r.status===200 && b.length>8 && b[0]===137 && b[1]===80 && b[2]===78 && b[3]===71;});null");
            await("window.__phoneResource!==null",10000);
            require(Boolean.TRUE.equals(js("window.__phoneResource")),"Authenticated Actor resource did not return the actual PNG");
        } finally {
            http("/v1/config","PUT",new JSONObject().put("providers",new JSONArray()).put("models",new JSONArray()).put("main_model",JSONObject.NULL).put("approval_model",JSONObject.NULL));
        }
    }
}
