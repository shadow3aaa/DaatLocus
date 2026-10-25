package io.shadow3aaa.daatlocus;

import android.app.Activity;
import android.app.Instrumentation;
import android.content.Intent;
import android.os.Bundle;
import android.os.SystemClock;
import android.view.View;
import android.view.ViewGroup;
import android.webkit.WebView;
import android.database.Cursor;
import android.database.sqlite.SQLiteDatabase;
import java.io.File;
import java.io.RandomAccessFile;
import org.json.JSONArray;
import org.json.JSONObject;
import org.json.JSONTokener;
import java.util.concurrent.CountDownLatch;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

// Platform-only instrumentation leaves the distributed APK's shrinker inputs unchanged.
public final class ReleaseStartupTest extends Instrumentation {
    private boolean syncDiagnostics;
    private boolean syncCheck;
    private Bundle arguments;
    @Override
    public void onCreate(Bundle arguments) {
        super.onCreate(arguments);
        this.arguments = arguments;
        syncDiagnostics = arguments != null && "sync".equals(arguments.getString("mode"));
        syncCheck = arguments != null && "check".equals(arguments.getString("mode"));
        start();
    }

    @Override
    public void onStart() {
        Bundle status = new Bundle();
        status.putString("id", "InstrumentationTestRunner");
        status.putString("class", getClass().getName());
        status.putString("test", arguments != null && "phone".equals(arguments.getString("mode")) ? "phoneUseNativeContract" : syncCheck ? "existingAccountSyncAndInput" : syncDiagnostics ? "existingAccountSync" : "freshInstallLoadsAccountLogin");
        status.putInt("numtests", 1);
        status.putInt("current", 1);
        if (arguments != null && "browser".equals(arguments.getString("mode"))) {
            status.putString("test", "browserBackgroundIsolation");
        }
        sendStatus(1, status);
        Bundle result = new Bundle();
        try {
            NetworkSecurityTest.check();
            Activity activity = startActivitySync(new Intent(Intent.ACTION_MAIN)
                .setClassName(getTargetContext(), "io.shadow3aaa.daatlocus.MainActivity")
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            if (arguments != null && "browser".equals(arguments.getString("mode"))) {
                result.putString("checks", BrowserBackgroundTest.run(this, (MainActivity) activity));
                sendStatus(0, status);
                result.putString("stream", "\nOK (BrowserUse background contract)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (arguments != null && "phone".equals(arguments.getString("mode"))) {
                result.putString("checks", PhoneUseTest.run(this, activity, arguments));
                sendStatus(0, status);
                result.putString("stream", "true".equals(arguments.getString("verify_disabled")) ? "\nOK (PhoneUse disabled persistence)\n" : "true".equals(arguments.getString("verify_display")) ? "\nOK (PhoneUse display persistence)\n" : "\nOK (PhoneUse native contract)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (syncDiagnostics || syncCheck) {
                SystemClock.sleep(5000);
                if (syncCheck) result.putString("check", checkExistingAccount(activity));
                result.putString("diagnostics", syncDiagnostics());
                result.putString("stream", "\nOK (device sync diagnostics)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            AtomicBoolean loaded = new AtomicBoolean(false);
            long deadline = SystemClock.elapsedRealtime() + 30_000;
            while (!loaded.get() && SystemClock.elapsedRealtime() < deadline) {
                CountDownLatch checked = new CountDownLatch(1);
                runOnMainSync(() -> {
                    WebView webView = findWebView(activity.getWindow().getDecorView());
                    if (webView == null) checked.countDown();
                    else webView.evaluateJavascript(
                        "document.querySelector('#dl-account-name') !== null && "
                            + "document.querySelector('#dl-account-connection') !== null", value -> {
                            loaded.set("true".equals(value));
                            checked.countDown();
                        });
                });
                checked.await(1, TimeUnit.SECONDS);
                if (!loaded.get()) SystemClock.sleep(100);
            }
            if (!loaded.get()) throw new AssertionError("Release APK did not load native account login");
            EdgeToEdgeTest.checkWindow(this, activity);
            sendStatus(0, status);
            result.putString("stream", "\nOK (1 test)\n");
            finish(Activity.RESULT_OK, result);
        } catch (Throwable error) {
            status.putString("stack", error.toString());
            sendStatus(-2, status);
            result.putString("stream", "\nFAILURES!!!\n" + error + "\n");
            finish(Activity.RESULT_CANCELED, result);
        }
    }

    // Exercises the normal Owner cookie/Origin API inside the main WebView.
    // Login is completed by the human on the phone; no credentials are read.
    private String checkExistingAccount(Activity activity) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + 120000;
        JSONObject state = new JSONObject();
        while (SystemClock.elapsedRealtime() < deadline) {
            evaluate(activity, "window.__dlSyncCheck=null;" + nativeFetch() + "('/v1/stage/status').then(async r=>{let d=await r.json();window.__dlSyncCheck={http:r.status,core:d.core||null};}).catch(()=>{window.__dlSyncCheck={http:0};});null");
            SystemClock.sleep(500);
            Object value = evaluate(activity, "JSON.stringify(window.__dlSyncCheck)");
            if (value instanceof String && !"null".equals(value)) {
                state = new JSONObject((String)value);
                JSONObject core = state.optJSONObject("core");
                if (state.optInt("http") == 200 && core != null && core.optBoolean("ready")) break;
            }
            SystemClock.sleep(1000);
        }
        if (!state.has("core") || state.isNull("core") || !state.getJSONObject("core").optBoolean("ready")) return state.toString();
        String key = "sync-device-test-" + java.util.UUID.randomUUID();
        evaluate(activity, "window.__dlSyncInput=null;" + nativeFetch() + "('/v1/actors/core/messages',{method:'POST',headers:{'Content-Type':'application/json','X-Daat-Request':'1','Idempotency-Key':'"+key+"'},body:JSON.stringify({content:'设备同步实测：请仅回复收到，不要委派任务或执行其他操作。',attachments:[]})}).then(async r=>{let d=await r.json();window.__dlSyncInput={http:r.status,id:d.id||null,status:d.status||null,code:d.code||null};}).catch(()=>{window.__dlSyncInput={http:0};});null");
        long inputDeadline = SystemClock.elapsedRealtime() + 30000;
        while (SystemClock.elapsedRealtime() < inputDeadline) {
            Object value = evaluate(activity, "JSON.stringify(window.__dlSyncInput)");
            if (value instanceof String && !"null".equals(value)) {
                state.put("input", new JSONObject((String)value));
                break;
            }
            SystemClock.sleep(500);
        }
        SystemClock.sleep(5000);
        return state.toString();
    }

    // Invoke the actual native transport from the main WebView. Diagnostic HTTP
    // calls must not accidentally target the Tauri static-asset protocol.
    static String nativeFetch() {
        return "(async function(path,options={}){"
            + "const call=window.__TAURI_INTERNALS__.invoke;"
            + "const headers=Object.entries(options.headers||{}).filter(([k])=>k.toLowerCase()!=='x-daat-request');"
            + "const id=await call('native_http_start',new Uint8Array(),"
            + "{headers:{'x-daat-http':JSON.stringify({path,method:options.method||'GET',headers})}});"
            + "try{if(options.body)await call('native_http_write',new TextEncoder().encode(options.body),{headers:{'x-daat-http-id':String(id)}});"
            + "const reply=await call('native_http_send',{id});const chunks=[];let size=0;"
            + "if(![204,205,304].includes(reply.status)){for(;;){const raw=await call('native_http_read',{id});"
            + "const chunk=new Uint8Array(raw);if(!chunk.length)break;size+=chunk.length;"
            + "if(size>65536)throw new Error('Diagnostic response too large');chunks.push(chunk);}}"
            + "return new Response([204,205,304].includes(reply.status)?null:new Blob(chunks),reply);"
            + "}finally{await call('native_http_cancel',{id});}})";
    }

    Object evaluate(Activity activity, String script) throws Exception {
        CountDownLatch done = new CountDownLatch(1);
        java.util.concurrent.atomic.AtomicReference<String> value = new java.util.concurrent.atomic.AtomicReference<>();
        runOnMainSync(() -> {
            WebView web = findWebView(activity.getWindow().getDecorView());
            if (web == null) done.countDown();
            else web.evaluateJavascript(script, reply -> { value.set(reply); done.countDown(); });
        });
        if (!done.await(2, TimeUnit.SECONDS) || value.get() == null) return null;
        return new JSONTokener(value.get()).nextValue();
    }

    // Signed, adb-invoked instrumentation only. Return bounded connection facts;
    // never export credential files, model definitions, passwords or message text.
    private String syncDiagnostics() throws Exception {
        File data = new File(getTargetContext().getApplicationInfo().dataDir);
        File service = new File(data, "service");
        JSONObject result = new JSONObject();
        File database = new File(service, "actors/actors.sqlite3");
        result.put("database_exists", database.isFile());
        if (database.isFile()) {
            try (SQLiteDatabase db = SQLiteDatabase.openDatabase(database.getPath(), null, SQLiteDatabase.OPEN_READONLY)) {
                JSONArray labels = new JSONArray();
                try (Cursor rows = db.rawQuery("SELECT stage,label FROM stage_device_labels ORDER BY stage LIMIT 64", null)) {
                    while (rows.moveToNext()) labels.put(new JSONObject().put("device", rows.getString(0)).put("label", rows.getString(1)));
                }
                result.put("labels", labels);
                JSONArray peers = new JSONArray();
                try (Cursor rows = db.rawQuery("SELECT id,record,revoked FROM stage_peers ORDER BY id LIMIT 64", null)) {
                    while (rows.moveToNext()) peers.put(new JSONObject().put("device", rows.getString(0))
                        .put("generation", new JSONObject(rows.getString(1)).optLong("generation"))
                        .put("revoked", rows.getInt(2)));
                }
                result.put("peers", peers);
                try (Cursor rows = db.rawQuery("SELECT value FROM core_meta WHERE key='head'", null)) {
                    if (rows.moveToFirst()) result.put("head", new JSONObject(rows.getString(0)));
                }
                try (Cursor rows = db.rawQuery("SELECT count(*) FROM messages WHERE actor_id='core'", null)) {
                    if (rows.moveToFirst()) result.put("core_messages", rows.getLong(0));
                }
            }
        }
        JSONArray errors = new JSONArray();
        File log = new File(service, "logs/daat-locus.log");
        if (log.isFile()) {
            try (RandomAccessFile reader = new RandomAccessFile(log, "r")) {
                reader.seek(Math.max(0, reader.length() - 262144));
                String line;
                while ((line = reader.readLine()) != null) {
                    if (line.contains("Native peer channel stopped") || line.contains("Public device connection failed")
                        || line.contains("Account signaling") || line.contains("Device transport could not start")) {
                        if (errors.length() == 20) errors.remove(0);
                        errors.put(line.substring(0, Math.min(line.length(), 1000)));
                    }
                }
            }
        }
        result.put("connection_errors", errors);
        return result.toString();
    }

    static WebView findWebView(View view) {
        if (view instanceof WebView) return (WebView) view;
        if (view instanceof ViewGroup) {
            ViewGroup group = (ViewGroup) view;
            for (int i = 0; i < group.getChildCount(); i++) {
                WebView found = findWebView(group.getChildAt(i));
                if (found != null) return found;
            }
        }
        return null;
    }
}
