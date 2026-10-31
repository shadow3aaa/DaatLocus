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
            if (arguments != null && "fixture-recovery-fork".equals(arguments.getString("mode"))) {
                status.putString("test", "explicitLocalRecoveryBranch");
                result.putString("checks", fixtureRecoveryFork(activity));
                result.putString("diagnostics", syncDiagnostics());
                sendStatus(0, status);
                result.putString("stream", "\nOK (native explicit recovery branch)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (arguments != null && "fixture-ui-cancel".equals(arguments.getString("mode"))) {
                result.putString("checks", fixtureUiCancel(activity));
                sendStatus(0, status);
                result.putString("stream", "\nOK (native UI cancellation and draft retention)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (arguments != null && "fixture-recovery".equals(arguments.getString("mode"))) {
                fixtureRecovery(activity);
                result.putString("diagnostics", syncDiagnostics());
                sendStatus(0, status);
                result.putString("stream", "\nOK (native relay interruption and recovery)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (arguments != null && "fixture-config".equals(arguments.getString("mode"))) {
                fixtureConfig(activity);
                JSONObject checked = new JSONObject(checkExistingAccount(activity));
                if (!checked.getJSONObject("core").optBoolean("ready") || !checked.has("input")
                    || checked.getJSONObject("input").optInt("http") / 100 != 2)
                    throw new AssertionError("Fixture core/input not ready: " + checked);
                checked.put("completion", fixtureCompletion(checked.getJSONObject("input").getString("id")));
                result.putString("check", checked.toString());
                result.putString("diagnostics", syncDiagnostics());
                sendStatus(0, status);
                result.putString("stream", "\nOK (native fixture configuration and input)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
            if (arguments != null && "fixture-login".equals(arguments.getString("mode"))) {
                fixtureLogin(activity);
                result.putString("checks", "Native WebUI OPAQUE fixture login completed");
                result.putString("diagnostics", syncDiagnostics());
                sendStatus(0, status);
                result.putString("stream", "\nOK (native fixture login)\n");
                finish(Activity.RESULT_OK, result);
                return;
            }
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

    private JSONObject fixtureStage(Activity activity) throws Exception {
        evaluate(activity, "window.__dlFixtureStage=null;" + nativeFetch()
            + "('/v1/stage').then(async r=>window.__dlFixtureStage={http:r.status,body:await r.json()}).catch(()=>window.__dlFixtureStage={http:0});null");
        long deadline = SystemClock.elapsedRealtime() + 10000;
        while (SystemClock.elapsedRealtime() < deadline) {
            Object value = evaluate(activity, "JSON.stringify(window.__dlFixtureStage)");
            if (value instanceof String && !"null".equals(value)) {
                JSONObject reply = new JSONObject((String) value);
                if (reply.optInt("http") != 200) throw new AssertionError("Fixture stage unavailable");
                JSONObject stage = reply.getJSONObject("body");
                if (!stage.optString("name").startsWith("dl-release-fixture-"))
                    throw new AssertionError("Recovery is limited to a disposable fixture account");
                return stage;
            }
            SystemClock.sleep(100);
        }
        throw new AssertionError("Fixture stage query timed out");
    }
    private void fixtureWait(Activity activity, String predicate, String failure) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + 30000;
        while (!Boolean.TRUE.equals(evaluate(activity, predicate))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError(failure);
            SystemClock.sleep(100);
        }
    }
    // Accessible names locate controls; assertions concern durable outcomes.
    private String fixtureButton(String first, String second, boolean twice) {
        return "(()=>{const labels=" + new JSONArray().put(first).put(second).toString()
            + ";const scope=document.querySelector('[role=dialog]')||document;"
            + "const button=[...scope.querySelectorAll('button')].find(node=>node.getClientRects().length&&labels.includes(node.textContent.trim()));"
            + "if(!button||button.disabled)return false;button.click();" + (twice ? "button.click();" : "") + "return true;})()";
    }
    private String fixtureRecoveryFork(Activity activity) throws Exception {
        fixtureWait(activity, "!!window.__TAURI_INTERNALS__", "Native shell unavailable");
        fixtureStage(activity);
        long sourceDeadline = SystemClock.elapsedRealtime() + 60000;
        while (true) {
            evaluate(activity, "window.__dlForkPreview=null;" + nativeFetch()
                + "('/v1/stage/recovery/fork').then(async r=>window.__dlForkPreview={http:r.status,body:await r.json()});null");
            fixtureWait(activity, "window.__dlForkPreview!==null", "Recovery preview timed out");
            JSONObject preview = new JSONObject((String)evaluate(activity, "JSON.stringify(window.__dlForkPreview)"));
            JSONObject body = preview.getJSONObject("body");
            if (preview.getInt("http") == 200 && body.optBoolean("eligible") && body.getJSONArray("sources").length() == 1
                && arguments.getString("source").equals(body.getJSONArray("sources").getJSONObject(0).getString("source"))) break;
            if (SystemClock.elapsedRealtime() > sourceDeadline) throw new AssertionError("Expected one authorized surviving fixture source");
            SystemClock.sleep(500);
        }
        evaluate(activity, "history.pushState(null,'','/settings/devices');dispatchEvent(new PopStateEvent('popstate'));null");
        fixtureWait(activity, fixtureButton("Review recovery", "查看恢复选项", false), "Recovery settings action unavailable");
        fixtureWait(activity, "!!document.querySelector('#dl-recovery-source')", "Recovery source did not load");
        fixtureWait(activity, fixtureButton("Cancel", "取消", false), "Recovery dialog could not be cancelled");
        fixtureWait(activity, "!document.querySelector('[role=dialog]')", "Cancelled recovery remained open");
        fixtureWait(activity, fixtureButton("Review recovery", "查看恢复选项", false), "Recovery dialog could not reopen");
        fixtureWait(activity, "!!document.querySelector('#dl-recovery-source')", "Recovery source did not reload");
        fixtureWait(activity, fixtureButton("Create local recovery branch", "创建本地恢复分支", true), "Recovery confirmation unavailable");
        long deadline = SystemClock.elapsedRealtime() + 120000;
        JSONObject core;
        while (true) {
            core = fixtureStage(activity).optJSONObject("core");
            if (core != null && core.optBoolean("ready") && "local_recovery".equals(core.optString("state"))
                && !core.optString("recovery_branch").isEmpty()) break;
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Explicit recovery did not activate a ready branch: " + core);
            SystemClock.sleep(200);
        }
        String history = arguments.getString("history");
        if (!"completed".equals(fixtureCompletion(history))) throw new AssertionError("Completed source history was not preserved");
        File database = new File(getTargetContext().getApplicationInfo().dataDir, "service/actors/actors.sqlite3");
        try (SQLiteDatabase db = SQLiteDatabase.openDatabase(database.getPath(), null, SQLiteDatabase.OPEN_READONLY);
             Cursor rows = db.rawQuery("SELECT count(*) FROM core_meta WHERE key LIKE 'fork_receipt/%'", null)) {
            if (!rows.moveToFirst() || rows.getInt(0) != 1) throw new AssertionError("Repeated confirmation created duplicate recovery receipts");
        }
        fixtureWait(activity, "!!document.querySelector('.dl-composer-textarea:not(:disabled)')", "Recovered composer unavailable");
        String content = "dl-release-fork-input-" + SystemClock.elapsedRealtime();
        evaluate(activity, "(()=>{const node=document.querySelector('.dl-composer-textarea');"
            + "Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(node," + JSONObject.quote(content)
            + ");node.dispatchEvent(new Event('input',{bubbles:true}));})();null");
        fixtureWait(activity, "!!document.querySelector('.dl-send-button:not(:disabled)')", "Recovered Send unavailable");
        evaluate(activity, "document.querySelector('.dl-send-button').click();null");
        JSONObject message;
        deadline = SystemClock.elapsedRealtime() + 30000;
        while ((message = fixtureMessage(content)) == null) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Recovered UI did not submit input");
            SystemClock.sleep(100);
        }
        fixtureCompletion(message.getString("id"));
        fixtureWait(activity, "!!document.getElementById(" + JSONObject.quote("message-core-" + message.getString("id"))
            + ")?.querySelector('[data-message-side=reply]')?.textContent.includes('fixture acknowledgment')", "Recovered reply did not reach the UI");
        return new JSONObject().put("branch", core.getString("recovery_branch")).put("history", history)
            .put("new_input", message.getString("id")).put("source", arguments.getString("source"))
            .put("dialog_cancel_reopen", true).put("duplicate_confirmation_clicks", 2).put("receipt_count", 1)
            .put("state", core.getString("state")).put("reply_visible", true).toString();
    }
    private JSONObject fixtureMessage(String content) throws Exception {
        File file = new File(getTargetContext().getApplicationInfo().dataDir, "service/actors/actors.sqlite3");
        try (SQLiteDatabase db = SQLiteDatabase.openDatabase(file.getPath(), null, SQLiteDatabase.OPEN_READONLY);
             Cursor rows = db.rawQuery("SELECT record FROM messages ORDER BY seq DESC LIMIT 16", null)) {
            while (rows.moveToNext()) {
                JSONObject message = new JSONObject(rows.getString(0));
                if (content.equals(message.optString("content"))) return message;
            }
        }
        return null;
    }
    private String fixtureUiCancel(Activity activity) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + 30000;
        while (!Boolean.TRUE.equals(evaluate(activity, "!!document.querySelector('.dl-composer-textarea:not(:disabled)')"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture Core composer unavailable");
            SystemClock.sleep(100);
        }
        fixtureStage(activity);
        String content = "dl-release-ui-cancel-" + SystemClock.elapsedRealtime();
        String draft = "Retain this unsent 0.6.0 fixture draft";
        String setText = "value=>{const node=document.querySelector('.dl-composer-textarea');Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(node,value);node.dispatchEvent(new Event('input',{bubbles:true}));}";
        evaluate(activity, "(" + setText + ")(" + JSONObject.quote(content) + ");null");
        while (!Boolean.TRUE.equals(evaluate(activity, "!!document.querySelector('.dl-send-button:not(:disabled)')"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture Send action unavailable");
            SystemClock.sleep(100);
        }
        evaluate(activity, "document.querySelector('.dl-send-button').click();null");
        JSONObject message;
        while (true) {
            message = fixtureMessage(content);
            if (message != null && "running".equals(message.getString("status"))
                && Boolean.TRUE.equals(evaluate(activity, "!!document.querySelector('.dl-stop-button:not(:disabled)')"))) break;
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture did not expose a running-task Stop action");
            SystemClock.sleep(100);
        }
        fixturePhase("ui-running");
        evaluate(activity, "(" + setText + ")(" + JSONObject.quote(draft) + ");null");
        while (!draft.equals(evaluate(activity, "document.querySelector('.dl-composer-textarea')?.value"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture draft was not rendered");
            SystemClock.sleep(100);
        }
        evaluate(activity, "(()=>{const stop=document.querySelector('.dl-stop-button');stop.click();stop.click();})();null");
        deadline = SystemClock.elapsedRealtime() + 30000;
        while (true) {
            message = fixtureMessage(content);
            String state = message.getString("status");
            if (state.equals("cancelled") || state.equals("interrupted")) break;
            if (state.equals("failed") || state.equals("completed")) throw new AssertionError("Fixture Stop did not cancel running work: " + state);
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture cancellation did not settle");
            SystemClock.sleep(100);
        }
        if (message.isNull("started_at_ms") || !message.optBoolean("cancel_requested"))
            throw new AssertionError("Fixture did not cancel an admitted running request");
        while (!Boolean.TRUE.equals(evaluate(activity, "!document.querySelector('.dl-stop-button') && !!document.querySelector('.dl-send-button:not(:disabled)')"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture UI did not recover after Stop");
            SystemClock.sleep(100);
        }
        if (!draft.equals(evaluate(activity, "document.querySelector('.dl-composer-textarea')?.value")))
            throw new AssertionError("Stop discarded the unsent fixture draft");
        if (!Boolean.TRUE.equals(evaluate(activity, "!!document.getElementById(" + JSONObject.quote("message-core-" + message.getString("id")) + ")")))
            throw new AssertionError("Stop cleared the visible conversation");
        return new JSONObject().put("message_id", message.getString("id")).put("status", message.getString("status"))
            .put("draft_retained", true).put("transcript_retained", true).put("repeated_stop_clicks", 2).put("send_reenabled", true).toString();
    }
    private void fixturePhase(String phase) {
        Bundle update = new Bundle(); update.putString("fixture_phase", phase); sendStatus(2, update);
    }
    private void fixtureRecovery(Activity activity) throws Exception {
        long deadline = SystemClock.elapsedRealtime() + 30000;
        while (!Boolean.TRUE.equals(evaluate(activity,
            "document.readyState==='complete' && !!window.__TAURI_INTERNALS__ && !!document.getElementById('root')?.childElementCount"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture native UI did not finish loading");
            SystemClock.sleep(100);
        }
        deadline = SystemClock.elapsedRealtime() + 30000;
        while (!"online".equals(fixtureStage(activity).getJSONObject("public_login").getString("state"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture relay was not initially online");
            SystemClock.sleep(200);
        }
        fixturePhase("ready");
        deadline = SystemClock.elapsedRealtime() + 60000;
        while ("online".equals(fixtureStage(activity).getJSONObject("public_login").getString("state"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Relay interruption was not observed");
            SystemClock.sleep(200);
        }
        fixturePhase("disconnected");
        deadline = SystemClock.elapsedRealtime() + 120000;
        while (true) {
            JSONObject stage = fixtureStage(activity);
            JSONObject core = stage.optJSONObject("core");
            if ("online".equals(stage.getJSONObject("public_login").getString("state"))
                && core != null && core.optBoolean("ready") && core.optBoolean("synchronized")) break;
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Relay did not recover a synchronized ready Core");
            SystemClock.sleep(200);
        }
        fixturePhase("reconnected");
        JSONObject checked = new JSONObject(checkExistingAccount(activity));
        if (!checked.has("input") || checked.getJSONObject("input").optInt("http") / 100 != 2)
            throw new AssertionError("Recovered Core did not admit new input: " + checked);
        String id = checked.getJSONObject("input").getString("id");
        fixtureCompletion(id);
        deadline = SystemClock.elapsedRealtime() + 30000;
        String visibleReply = "(()=>{const turn=document.getElementById(" + JSONObject.quote("message-core-" + id)
            + ");return !!turn?.querySelector('[data-message-side=reply]')?.textContent.includes('fixture acknowledgment');})()";
        while (!Boolean.TRUE.equals(evaluate(activity, visibleReply))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Recovered message completed but its reply did not reach the native UI");
            SystemClock.sleep(100);
        }
        fixturePhase("ui-reply-observed");
    }

    private String fixtureCompletion(String id) throws Exception {
        File database = new File(getTargetContext().getApplicationInfo().dataDir, "service/actors/actors.sqlite3");
        long deadline = SystemClock.elapsedRealtime() + 60000;
        while (SystemClock.elapsedRealtime() < deadline) {
            try (SQLiteDatabase db = SQLiteDatabase.openDatabase(database.getPath(), null, SQLiteDatabase.OPEN_READONLY);
                 Cursor rows = db.rawQuery("SELECT record FROM messages WHERE id=?", new String[]{id})) {
                if (rows.moveToFirst()) {
                    JSONObject message = new JSONObject(rows.getString(0));
                    String state = message.getString("status");
                    if (state.equals("failed") || state.equals("cancelled"))
                        throw new AssertionError("Fixture message did not complete: " + state + ": " + message.optString("error"));
                    if (state.equals("completed")) return state;
                }
            }
            SystemClock.sleep(100);
        }
        throw new AssertionError("Fixture terminal state did not replicate to Android");
    }

    private void fixtureConfig(Activity activity) throws Exception {
        JSONObject config = new JSONObject(new String(android.util.Base64.decode(
            arguments.getString("fixture"), android.util.Base64.NO_WRAP), java.nio.charset.StandardCharsets.UTF_8));
        JSONArray providers = config.getJSONArray("providers");
        if (providers.length() != 1 || !providers.getJSONObject(0).getString("name").startsWith("fixture-")
            || !providers.getJSONObject(0).getString("base_url").startsWith("http://127.0.0.1:"))
            throw new AssertionError("Only a disposable loopback model may be configured");
        long deadline = SystemClock.elapsedRealtime() + 30000;
        while (!Boolean.TRUE.equals(evaluate(activity, "!!window.__TAURI_INTERNALS__"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Native shell unavailable");
            SystemClock.sleep(100);
        }
        fixtureStage(activity); // Never change a personal account's model settings.
        evaluate(activity, "window.__dlFixtureConfig=null;" + nativeFetch()
            + "('/v1/config',{method:'PUT',headers:{'Content-Type':'application/json'},body:"
            + JSONObject.quote(config.toString()) + "}).then(async r=>window.__dlFixtureConfig={http:r.status,body:await r.json()}).catch(()=>window.__dlFixtureConfig={http:0});null");
        while (SystemClock.elapsedRealtime() < deadline) {
            Object value = evaluate(activity, "JSON.stringify(window.__dlFixtureConfig)");
            if (value instanceof String && !"null".equals(value)) {
                JSONObject reply = new JSONObject((String) value);
                if (reply.optInt("http") / 100 != 2) throw new AssertionError("Fixture model configuration rejected: " + reply);
                return;
            }
            SystemClock.sleep(100);
        }
        throw new AssertionError("Fixture model configuration timed out");
    }

    // Explicit disposable loopback fixtures only. This drives the shipped
    // React form and OPAQUE WASM; no credential files or cookies are imported.
    private void fixtureLogin(Activity activity) throws Exception {
        JSONObject fixture = new JSONObject(new String(android.util.Base64.decode(
            arguments.getString("fixture"), android.util.Base64.NO_WRAP), java.nio.charset.StandardCharsets.UTF_8));
        String relay = fixture.getString("relay"), name = fixture.getString("name");
        if (!relay.startsWith("ws://127.0.0.1:") || !name.startsWith("dl-release-fixture-"))
            throw new AssertionError("Use an explicitly disposable loopback account");
        long deadline = SystemClock.elapsedRealtime() + 30000;
        while (!Boolean.TRUE.equals(evaluate(activity, "!!document.querySelector('#dl-account-name')"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture login form did not load");
            SystemClock.sleep(100);
        }
        // Configure the existing OS-owner bootstrap after the form loaded.
        // The production UI requires WSS; the backend's existing loopback-only
        // WS test transport is configured without weakening TLS or changing UI.
        evaluate(activity, "window.__dlFixtureConnection=null;window.__TAURI_INTERNALS__.invoke('desktop_account',{input:{kind:'connection',relay_origin:"
            + JSONObject.quote(relay) + "}}).then(r=>window.__dlFixtureConnection=r.status).catch(()=>window.__dlFixtureConnection=0);null");
        while (!Integer.valueOf(204).equals(evaluate(activity, "window.__dlFixtureConnection"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture connection setup failed");
            SystemClock.sleep(100);
        }
        evaluate(activity, "(()=>{const set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;"
            + "for(const [id,value] of " + new JSONArray().put(new JSONArray().put("dl-account-name").put(name))
                .put(new JSONArray().put("dl-access-credential").put(fixture.getString("password"))).toString()
            + "){const node=document.getElementById(id);set.call(node,value);node.dispatchEvent(new Event('input',{bubbles:true}));}return true})()");
        while (!Boolean.TRUE.equals(evaluate(activity, "document.querySelector('button[type=submit]')?.disabled===false"))) {
            if (SystemClock.elapsedRealtime() > deadline) throw new AssertionError("Fixture form remained disabled");
            SystemClock.sleep(100);
        }
        evaluate(activity, "document.querySelector('form').requestSubmit();null");
        deadline = SystemClock.elapsedRealtime() + 120000;
        while (SystemClock.elapsedRealtime() < deadline) {
            if (Boolean.TRUE.equals(evaluate(activity, "!document.querySelector('#dl-account-name')"))) return;
            SystemClock.sleep(200);
        }
        throw new AssertionError("Native fixture OPAQUE login did not complete");
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
        evaluate(activity, "window.__dlSyncInput=null;" + nativeFetch() + "('/v1/core/messages',{method:'POST',headers:{'Content-Type':'application/json','X-Daat-Request':'1','Idempotency-Key':'"+key+"'},body:JSON.stringify({content:'设备同步实测：请仅回复收到，不要委派任务或执行其他操作。',attachments:[]})}).then(async r=>{let d=await r.json();window.__dlSyncInput={http:r.status,id:d.id||null,status:d.status||null,code:d.code||null};}).catch(()=>{window.__dlSyncInput={http:0};});null");
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
