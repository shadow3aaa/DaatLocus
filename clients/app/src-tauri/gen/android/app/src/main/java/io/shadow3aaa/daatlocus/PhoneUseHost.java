package io.shadow3aaa.daatlocus;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.app.ActivityManager;
import android.app.UiAutomation;
import android.content.AttributionSource;
import android.content.ComponentName;
import android.content.Context;
import android.content.ContextWrapper;
import android.content.Intent;
import android.content.pm.ResolveInfo;
import android.graphics.Bitmap;
import android.graphics.PixelFormat;
import android.graphics.Rect;
import android.hardware.display.DisplayManager;
import android.hardware.display.VirtualDisplay;
import android.media.Image;
import android.media.ImageReader;
import android.os.Bundle;
import android.os.Handler;
import android.os.HandlerThread;
import android.os.Process;
import android.os.SystemClock;
import android.view.KeyEvent;
import android.view.MotionEvent;
import android.view.accessibility.AccessibilityNodeInfo;
import android.view.accessibility.AccessibilityWindowInfo;
import androidx.annotation.Keep;
import org.json.JSONArray;
import org.json.JSONObject;
import java.io.ByteArrayOutputStream;
import java.lang.reflect.Constructor;
import java.util.*;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;

/** One privileged device backend. No arbitrary shell, paths, Binder methods or display IDs
 * are accepted from tools. Every Actor gets its own display, including its modal windows. */
@Keep
public final class PhoneUseHost {
    private static final Object LOCK = new Object();
    private static final int FLAGS = 0x1fd4b;
    private static final Map<String, Workspace> spaces = new LinkedHashMap<>();
    private static final Set<String> cancelled = ConcurrentHashMap.newKeySet();
    private static final Map<String, ComponentName> apps = new LinkedHashMap<>();
    private static final Map<String, String> names = new HashMap<>();
    private static UiAutomation automation;
    private static HandlerThread accessThread;
    private static HandlerThread captureThread;
    private static Context shell;
    private static boolean effect;

    static String id() { return UUID.randomUUID().toString(); }
    static void require(boolean condition, String message) { if (!condition) throw new IllegalStateException(message); }
    static JSONObject object(Object... values) throws Exception {
        JSONObject result = new JSONObject();
        for (int i = 0; i < values.length; i += 2) result.put((String) values[i], values[i + 1] == null ? JSONObject.NULL : values[i + 1]);
        return result;
    }
    static JSONObject rect(Rect rect) throws Exception { return object("x", rect.left, "y", rect.top, "width", rect.width(), "height", rect.height()); }
    static ComputerReply reply(JSONObject payload, byte[] image, String failure, boolean resources) throws Exception {
        return new ComputerReply(object("summary", failure == null ? "PhoneUse" : "PhoneUse requires observation",
            "payload", payload, "failure", failure, "has_resources", resources).toString(), image == null ? null : ComputerImages.INSTANCE.pipe(image));
    }

    public static ComputerReply handle(Context context, String request) {
        try {
            require(request.length() <= 65536, "Request exceeds limit");
            JSONObject cmd = new JSONObject(request);
            String op = cmd.getString("op");
            if (op.equals("cancel")) {
                if (cancelled.size() < 1024) cancelled.add(cmd.getString("id"));
                return reply(object("status", "cancelled"), null, null, false);
            }
            synchronized (LOCK) {
                effect = false;
                Workspace space = null;
                String requestId = cmd.optString("id");
                try {
                    // Owner observation never initializes, acquires or switches a display.
                    if (op.equals("preview_list")) {
                        JSONArray rows = new JSONArray();
                        for (Map.Entry<String, Workspace> entry : spaces.entrySet()) {
                            Workspace current = entry.getValue(); current.validate();
                            rows.put(object("id", current.previewId, "actor_id", entry.getKey(), "name", null,
                                "width", current.width, "height", current.height));
                        }
                        return reply(object("displays", rows), null, null, !spaces.isEmpty());
                    }
                    if (op.equals("preview_frame")) {
                        String selected = cmd.getString("display_id");
                        for (Workspace current : spaces.values()) if (current.previewId.equals(selected)) {
                            return reply(object("status", "observed"), current.preview(), null, !spaces.isEmpty());
                        }
                        throw new IllegalStateException("Virtual display no longer exists");
                    }
                    if (op.equals("stop")) { close(); return reply(object("status", "stopped"), null, null, false); }
                    initialize(context);
                    if (op.equals("probe")) {
                        try (Workspace probe = new Workspace("probe", "probe", PhoneUseDisplay.fromJson(cmd.getJSONObject("display")))) { probe.validate(); }
                        return reply(object("status", "ready", "uid", Process.myUid()), null, null, false);
                    }
                    if (op.equals("configure_display")) {
                        PhoneUseDisplay config = PhoneUseDisplay.fromJson(cmd.getJSONObject("display"));
                        require((long) spaces.size() * config.width * config.height <= PhoneUseDisplay.MAX_PIXELS, "Virtual display pixel capacity reached");
                        Map<Workspace, PhoneUseDisplay> changed = new LinkedHashMap<>();
                        try {
                            for (Workspace workspace : spaces.values()) {
                                PhoneUseDisplay previous = new PhoneUseDisplay(workspace.width, workspace.height, workspace.density);
                                effect = true; workspace.resize(config); changed.put(workspace, previous);
                            }
                        } catch (Exception error) {
                            for (Map.Entry<Workspace, PhoneUseDisplay> entry : changed.entrySet()) {
                                try { entry.getKey().resize(entry.getValue()); }
                                catch (Exception rollback) { entry.getKey().broken = true; }
                            }
                            boolean broken = spaces.values().stream().anyMatch(workspace -> workspace.broken);
                            return reply(object("status", "outcome_unknown", "reconnect_required", broken), null, "Display adjustment failed; observe again or reconnect", true);
                        }
                        return reply(object("status", "applied"), null, null, !spaces.isEmpty());
                    }
                    String actor = cmd.getString("actor"), session = cmd.getString("session");
                    require(actor.matches("[a-zA-Z0-9_-]{1,128}") && session.length() <= 128, "Invalid Actor reference");
                    space = spaces.get(actor);
                    if (op.equals("detach")) {
                        if (space != null && space.session.equals(session)) {
                            space.revoke();
                            if (!space.hasTasks()) { space.close(); spaces.remove(actor); }
                        }
                        return reply(object("status", "detached"), null, null, false);
                    }
                    checkCancelled(requestId);
                    long epoch = Long.parseLong(cmd.getString("epoch"));
                    if (space != null) { require(epoch >= space.epoch, "Stale execution epoch"); space.epoch = epoch; space.validate(); }
                    if (space != null && !space.session.equals(session)) { space.revoke(); space.session = session; }
                    JSONObject args = cmd.getJSONObject("args");
                    if (op.equals("list_apps")) {
                        discoverApps();
                        JSONArray rows = new JSONArray();
                        String query = args.optString("query", "").toLowerCase(Locale.ROOT);
                        if (query.equals("null")) query = "";
                        for (String app : apps.keySet()) if (names.get(app).toLowerCase(Locale.ROOT).contains(query)) rows.put(object("app_id", app, "name", names.get(app)));
                        return reply(page("apps", rows, args), null, null, space != null && space.hasTasks());
                    }
                    if (op.equals("launch_app")) {
                        ComponentName component = apps.get(args.getString("app_id"));
                        require(component != null, "Unknown app; list_apps first");
                        android.content.pm.ActivityInfo target = shell.getPackageManager().getActivityInfo(component, 0);
                        require(target.enabled && target.exported && target.applicationInfo.enabled, "Launcher activity is unavailable");
                        require(target.launchMode != android.content.pm.ActivityInfo.LAUNCH_SINGLE_TASK
                            && target.launchMode != android.content.pm.ActivityInfo.LAUNCH_SINGLE_INSTANCE,
                            "This launch mode can reuse a main-screen task and cannot be launched in isolation");
                        if (space == null) {
                            require(spaces.size() < 8, "Virtual display capacity reached");
                            space = new Workspace(actor, session, PhoneUseDisplay.fromJson(cmd.getJSONObject("display"))); space.epoch = epoch; spaces.put(actor, space);
                        }
                        Set<Integer> before = taskIds(-1);
                        checkCancelled(requestId);
                        effect = true;
                        java.lang.Process launch = new ProcessBuilder("/system/bin/am", "start", "-W", "--display", Integer.toString(space.display),
                            "-n", component.flattenToString(), "-a", Intent.ACTION_MAIN, "-c", Intent.CATEGORY_LAUNCHER, "-f", "0x18000000")
                            .redirectErrorStream(true).start();
                        // Drain concurrently; launch receipts and platform diagnostics are private.
                        Thread drain = new Thread(() -> { try { launch.getInputStream().transferTo(java.io.OutputStream.nullOutputStream()); } catch (Exception ignored) {} }, "dl-launch-output");
                        drain.setDaemon(true); drain.start();
                        if (!launch.waitFor(10, TimeUnit.SECONDS)) { launch.destroy(); throw new IllegalStateException("Launch outcome unknown; do not replay"); }
                        long deadline = SystemClock.uptimeMillis() + 5000;
                        String candidate = null;
                        Rect stableBounds = null;
                        do {
                            checkCancelled(requestId); space.discover();
                            List<Window> found = new ArrayList<>();
                            for (Window window : space.windows.values()) if (window.pkg.equals(component.getPackageName()) && !before.contains(window.task)) found.add(window);
                            if (found.size() == 1) {
                                Window window = found.get(0);
                                if (window.id.equals(candidate) && window.bounds.equals(stableBounds)) {
                                    window.taken = true;
                                    try { return space.observe(window, null, 1, "observed"); }
                                    catch (IllegalStateException transition) {
                                        // Observe the same launch while its windows settle. Never launch again.
                                        checkCancelled(requestId);
                                    }
                                }
                                candidate = window.id; stableBounds = new Rect(window.bounds);
                            } else { candidate = null; stableBounds = null; }
                            Thread.sleep(100);
                        } while (SystemClock.uptimeMillis() < deadline);
                        throw new IllegalStateException("No unique new task confirmed on the virtual display; inspect windows, do not replay launch");
                    }
                    if (op.equals("list_windows")) {
                        JSONArray rows = new JSONArray();
                        if (space != null) {
                            space.discover();
                            for (Window window : space.windows.values()) {
                                String app = appFor(window.pkg);
                                if (!args.isNull("app_id") && !args.getString("app_id").equals(app)) continue;
                                String query = args.isNull("query") ? "" : args.getString("query");
                                if (!window.title.toLowerCase(Locale.ROOT).contains(query.toLowerCase(Locale.ROOT))) continue;
                                rows.put(object("window_id", window.id, "app_id", app, "title", window.title, "taken", window.taken));
                            }
                        }
                        return reply(page("windows", rows, args), null, null, space != null && space.hasTasks());
                    }
                    require(space != null, "Launch an app in your virtual display first");
                    Window window = space.windows.get(args.getString("window_id"));
                    require(window != null, "Unknown or revoked window; list_windows again");
                    if (op.equals("release_window")) {
                        space.windows.remove(window.id); window.state = null; window.receiver = null;
                        return reply(object("status", "released", "input_revoked", true), null, null, space.hasTasks());
                    }
                    space.current(window);
                    if (op.equals("open_window")) { window.taken = true; return space.observe(window, null, 1, "observed"); }
                    require(window.taken, "Explicitly open this window before input or observation");
                    if (op.equals("get_window_state")) {
                        Rect region = null;
                        if (!args.isNull("region")) { JSONObject r = args.getJSONObject("region"); int x = r.getInt("x"), y = r.getInt("y"); region = new Rect(x, y, Math.addExact(x, r.getInt("width")), Math.addExact(y, r.getInt("height"))); }
                        return space.observe(window, region, args.isNull("scale") ? 1 : args.getInt("scale"), "observed");
                    }
                    require(args.getString("state_id").equals(window.state), "Stale observation; observe again");
                    space.unobscured(window);
                    checkCancelled(requestId);
                    if (op.equals("click")) {
                        require(args.getString("button").equals("left") && args.getInt("click_count") == 1, "Only single left taps are supported");
                        int[] p = window.point(args.getInt("x"), args.getInt("y"));
                        AccessibilityNodeInfo receiver = editableAt(space.current(window).getRoot(), p[0], p[1], 0, new int[]{0});
                        window.state = null; effect = true;
                        gesture(space, new int[][]{p, p}, requestId);
                        window.receiver = receiver;
                    } else if (op.equals("set_text")) {
                        String text = args.getString("text");
                        require(text.length() <= 4096 && text.indexOf('\0') < 0, "Text exceeds limit or contains NUL");
                        AccessibilityNodeInfo receiver = window.receiver;
                        require(receiver != null && receiver.refresh() && receiver.getWindowId() == window.platform
                            && window.pkg.contentEquals(receiver.getPackageName()) && receiver.isEditable() && receiver.isEnabled()
                            && receiver.getActionList().contains(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_TEXT), "Selected editable receiver unavailable; tap a supported control");
                        Bundle bundle = new Bundle(); bundle.putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text);
                        window.state = null; effect = true;
                        require(receiver.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, bundle), "Text action was not accepted; observe before retrying");
                    } else if (op.equals("press_key")) {
                        Map<String, Integer> keys = new HashMap<>();
                        keys.put("Back", KeyEvent.KEYCODE_BACK); keys.put("Enter", KeyEvent.KEYCODE_ENTER); keys.put("Escape", KeyEvent.KEYCODE_ESCAPE);
                        keys.put("Backspace", KeyEvent.KEYCODE_DEL); keys.put("Delete", KeyEvent.KEYCODE_FORWARD_DEL);
                        keys.put("ArrowLeft", KeyEvent.KEYCODE_DPAD_LEFT); keys.put("ArrowRight", KeyEvent.KEYCODE_DPAD_RIGHT);
                        keys.put("ArrowUp", KeyEvent.KEYCODE_DPAD_UP); keys.put("ArrowDown", KeyEvent.KEYCODE_DPAD_DOWN);
                        Integer key = keys.get(args.getString("key")); require(key != null, "Unsupported Android key");
                        require(space.current(window).isFocused(), "Window no longer has this display's focus");
                        window.state = null; effect = true;
                        long now = SystemClock.uptimeMillis();
                        inject(new KeyEvent(now, now, KeyEvent.ACTION_DOWN, key, 0), space.display);
                        inject(new KeyEvent(now, SystemClock.uptimeMillis(), KeyEvent.ACTION_UP, key, 0), space.display);
                    } else if (op.equals("drag")) {
                        JSONArray path = args.getJSONArray("path"); require(path.length() >= 2 && path.length() <= 128, "Gesture needs 2..128 points");
                        int[][] points = new int[path.length()][];
                        for (int i = 0; i < path.length(); i++) points[i] = window.point(path.getJSONObject(i).getInt("x"), path.getJSONObject(i).getInt("y"));
                        window.state = null; window.receiver = null; effect = true; gesture(space, points, requestId);
                    } else if (op.equals("scroll")) {
                        require(args.getInt("delta_x") == 0 && Math.abs((long) args.getInt("delta_y")) <= 20, "Only bounded vertical scrolling is supported");
                        int[] p = window.point(args.getInt("x"), args.getInt("y"));
                        int amount = Math.max(-window.bounds.height() / 2, Math.min(window.bounds.height() / 2, args.getInt("delta_y") * 48));
                        int end = Math.max(window.bounds.top + 1, Math.min(window.bounds.bottom - 1, p[1] - amount));
                        window.state = null; window.receiver = null; effect = true; gesture(space, new int[][]{p, {p[0], end}}, requestId);
                    } else if (op.equals("close_window")) {
                        require(taskIds(space.display).contains(window.task), "Task left the owned display");
                        Set<Integer> processes = processIds(window.pkg);
                        window.state = null; effect = true;
                        Object service = taskService();
                        require((Boolean) service.getClass().getMethod("removeTask", int.class).invoke(service, window.task), "Task closure unconfirmed");
                        space.windows.values().removeIf(w -> w.task == window.task);
                        // Android removes a last task before its queued cached-process kill.
                        // Wait for that existing process to leave, rather than starting a new
                        // task into a process which is already being removed. No process signal
                        // or force-stop is sent. Other tasks of the package remain untouched.
                        long closing = SystemClock.uptimeMillis() + 3000;
                        while (SystemClock.uptimeMillis() < closing && !Collections.disjoint(processes, processIds(window.pkg))
                                && !hasOtherTask(window.pkg, window.task)) Thread.sleep(30);
                        return reply(object("status", "dispatched", "input_revoked", true), null, null, space.hasTasks());
                    } else throw new IllegalArgumentException("Unsupported PhoneUse operation");
                    Thread.sleep(150);
                    checkCancelled(requestId);
                    return space.observe(window, null, 1, "dispatched");
                } catch (Throwable error) {
                    String message = error.getMessage();
                    if (message == null) message = "Android capability unavailable";
                    // No raw platform exception stacks, private paths or command diagnostics in observations.
                    return reply(object("status", effect ? "outcome_unknown" : "not_started", "error", message), null, message,
                        space != null && space.hasTasks());
                } finally { cancelled.remove(requestId); }
            }
        } catch (Throwable error) { return ComputerImages.INSTANCE.error("Invalid or unavailable PhoneUse request", "not_started"); }
    }
    static void checkCancelled(String id) { require(!cancelled.contains(id) && !Thread.currentThread().isInterrupted(), "Operation cancelled; observe before continuing"); }

    static void initialize(Context context) throws Exception {
        require(Process.myUid() == 0 || Process.myUid() == 2000, "PhoneUse requires authorized root or ADB identity");
        require(android.os.Build.VERSION.SDK_INT >= 35, "This Android version does not support the verified display isolation policy");
        if (shell != null) return;
        Class<?> thread = Class.forName("android.app.ActivityThread");
        Object main = thread.getMethod("currentActivityThread").invoke(null);
        if (main == null) main = thread.getMethod("systemMain").invoke(null);
        Context system = (Context) thread.getMethod("getSystemContext").invoke(main);
        shell = new ContextWrapper(system) {
            @Override public String getPackageName() { return "com.android.shell"; }
            @Override public String getOpPackageName() { return "com.android.shell"; }
            @Override public AttributionSource getAttributionSource() { return new AttributionSource.Builder(2000).setPackageName("com.android.shell").build(); }
            @Override public Context getApplicationContext() { return this; }
        };
        captureThread = new HandlerThread("dl-vd-capture"); captureThread.start();
        accessThread = new HandlerThread("dl-vd-access"); accessThread.start();
        Class<?> iface = Class.forName("android.app.IUiAutomationConnection");
        Object connection = Class.forName("android.app.UiAutomationConnection").getDeclaredConstructor().newInstance();
        Constructor<UiAutomation> constructor = UiAutomation.class.getDeclaredConstructor(android.os.Looper.class, iface);
        constructor.setAccessible(true); automation = constructor.newInstance(accessThread.getLooper(), connection);
        UiAutomation.class.getDeclaredMethod("connect", int.class).invoke(automation, 1);
        AccessibilityServiceInfo info = automation.getServiceInfo();
        info.flags |= AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS | AccessibilityServiceInfo.FLAG_INCLUDE_NOT_IMPORTANT_VIEWS;
        automation.setServiceInfo(info);
    }
    public static void close() {
        synchronized (LOCK) {
            for (Workspace space : spaces.values()) space.close(); spaces.clear(); apps.clear(); names.clear();
            if (automation != null) try { UiAutomation.class.getDeclaredMethod("disconnect").invoke(automation); } catch (Exception ignored) {}
            automation = null; shell = null;
            if (accessThread != null) accessThread.quitSafely(); if (captureThread != null) captureThread.quitSafely();
            accessThread = null; captureThread = null;
        }
    }
    static boolean ownPackage(String pkg) {
        // A previous client may remain installed alongside the renamed package.
        return pkg.equals(BuildConfig.APPLICATION_ID) || pkg.equals("io.daat_locus.app");
    }
    static void discoverApps() throws Exception {
        for (ResolveInfo info : shell.getPackageManager().queryIntentActivities(new Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER), 0)) {
            if (ownPackage(info.activityInfo.packageName) || !info.activityInfo.enabled || !info.activityInfo.exported) continue;
            ComponentName component = new ComponentName(info.activityInfo.packageName, info.activityInfo.name);
            String existing = null; for (String key : apps.keySet()) if (apps.get(key).equals(component)) existing = key;
            if (existing == null) { existing = id(); apps.put(existing, component); }
            names.put(existing, info.loadLabel(shell.getPackageManager()).toString());
        }
    }
    static String appFor(String pkg) { for (String key : apps.keySet()) if (apps.get(key).getPackageName().equals(pkg)) return key; return null; }
    static JSONObject page(String field, JSONArray rows, JSONObject args) throws Exception {
        int limit = args.getInt("limit"); require(limit >= 1 && limit <= 100, "Page limit must be 1..100");
        int offset = args.isNull("cursor") ? 0 : Integer.parseInt(args.getString("cursor")); require(offset >= 0 && offset <= rows.length(), "Invalid cursor");
        JSONArray page = new JSONArray(); for (int i = offset; i < Math.min(rows.length(), offset + limit); i++) page.put(rows.get(i));
        return object(field, page, "next_cursor", offset + limit < rows.length() ? Integer.toString(offset + limit) : null);
    }
    static Object taskService() throws Exception { return Class.forName("android.app.ActivityTaskManager").getMethod("getService").invoke(null); }
    @SuppressWarnings("unchecked") static List<ActivityManager.RunningTaskInfo> tasks(int display) throws Exception {
        Object service = taskService();
        return (List<ActivityManager.RunningTaskInfo>) service.getClass().getMethod("getTasks", int.class, boolean.class, boolean.class, int.class).invoke(service, 256, false, false, display);
    }
    static Set<Integer> taskIds(int display) throws Exception {
        Set<Integer> ids = new HashSet<>(); for (ActivityManager.RunningTaskInfo task : tasks(display)) ids.add(task.taskId); return ids;
    }
    static boolean hasOtherTask(String pkg, int excluded) throws Exception {
        for (ActivityManager.RunningTaskInfo task : tasks(-1)) if (task.taskId != excluded &&
                ((task.baseActivity != null && pkg.equals(task.baseActivity.getPackageName())) ||
                 (task.topActivity != null && pkg.equals(task.topActivity.getPackageName())))) return true;
        return false;
    }
    static Set<Integer> processIds(String pkg) {
        Set<Integer> ids = new HashSet<>();
        List<ActivityManager.RunningAppProcessInfo> processes = shell.getSystemService(ActivityManager.class).getRunningAppProcesses();
        if (processes != null) for (ActivityManager.RunningAppProcessInfo process : processes)
            if (process.pkgList != null && Arrays.asList(process.pkgList).contains(pkg)) ids.add(process.pid);
        return ids;
    }
    // Runs only inside UID 0/2000 app_process, which is exempt from app hidden-API
    // enforcement. Never invoke this from the normal application UID. Routing to
    // a validated owned display is mandatory; there is no default-display fallback.
    @android.annotation.SuppressLint("BlockedPrivateApi")
    static void inject(android.view.InputEvent event, int display) throws Exception {
        android.view.InputEvent.class.getDeclaredMethod("setDisplayId", int.class).invoke(event, display);
        require(automation.injectInputEvent(event, true), "Input dispatch unconfirmed; observe before continuing");
    }
    static void gesture(Workspace space, int[][] points, String request) throws Exception {
        long down = SystemClock.uptimeMillis(); int[] last = points[0]; boolean pressed = false;
        try {
            touch(space.display, down, MotionEvent.ACTION_DOWN, last); pressed = true;
            for (int segment = 1; segment < points.length; segment++) {
                int[] start = points[segment - 1], end = points[segment];
                int samples = Math.max(1, Math.min(30, Math.max(Math.abs(end[0] - start[0]), Math.abs(end[1] - start[1])) / 16));
                for (int step = 1; step <= samples; step++) {
                    checkCancelled(request); space.validate();
                    last = new int[]{start[0] + (end[0] - start[0]) * step / samples, start[1] + (end[1] - start[1]) * step / samples};
                    touch(space.display, down, MotionEvent.ACTION_MOVE, last); Thread.sleep(8);
                }
            }
            touch(space.display, down, MotionEvent.ACTION_UP, last); pressed = false;
        } finally { if (pressed) touch(space.display, down, MotionEvent.ACTION_CANCEL, last); }
    }
    static void touch(int display, long down, int action, int[] point) throws Exception {
        MotionEvent event = MotionEvent.obtain(down, SystemClock.uptimeMillis(), action, point[0], point[1], 0);
        event.setSource(android.view.InputDevice.SOURCE_TOUCHSCREEN);
        try { inject(event, display); } finally { event.recycle(); }
    }
    static AccessibilityNodeInfo editableAt(AccessibilityNodeInfo node, int x, int y, int depth, int[] count) {
        if (node == null || depth > 50 || ++count[0] > 2048) return null;
        Rect bounds = new Rect(); node.getBoundsInScreen(bounds);
        if (!bounds.contains(x, y) || !node.isVisibleToUser()) return null;
        for (int i = 0; i < node.getChildCount(); i++) { AccessibilityNodeInfo found = editableAt(node.getChild(i), x, y, depth + 1, count); if (found != null) return found; }
        return node.isEditable() ? node : null;
    }
    static final class Window {
        String id = id(), state, title, pkg;
        int platform, task, scale = 1;
        Rect bounds, region;
        boolean taken;
        AccessibilityNodeInfo root, receiver;
        int[] point(int x, int y) {
            require(region != null && x >= 0 && y >= 0 && x < region.width() * scale && y < region.height() * scale, "Point outside observed image");
            return new int[]{bounds.left + region.left + x / scale, bounds.top + region.top + y / scale};
        }
    }
    static final class Workspace implements AutoCloseable {
        final VirtualDisplay vd;
        volatile ImageReader reader;
        final int display;
        int width, height, density;
        final String uniqueId;
        final String previewId = id();
        final Map<String, Window> windows = new LinkedHashMap<>();
        String session;
        long epoch;
        Bitmap latest;
        boolean closed;
        boolean broken;
        Workspace(String actor, String session, PhoneUseDisplay config) throws Exception {
            this.session = session;
            width = config.width; height = config.height; density = config.density;
            long pixels = (long) width * height;
            for (Workspace workspace : spaces.values()) pixels += (long) workspace.width * workspace.height;
            require(pixels <= PhoneUseDisplay.MAX_PIXELS, "Virtual display pixel capacity reached");
            reader = createReader(config);
            Constructor<DisplayManager> constructor = DisplayManager.class.getDeclaredConstructor(Context.class); constructor.setAccessible(true);
            VirtualDisplay created = null;
            try {
                created = constructor.newInstance(shell).createVirtualDisplay("DaatLocus-" + id(), width, height, config.density, reader.getSurface(), FLAGS);
                require(created != null, "Cannot create isolated virtual display");
                vd = created; display = vd.getDisplay().getDisplayId();
                // Text uses the selected accessibility receiver. Never let a
                // virtual editor connect to or relocate the main-screen IME.
                Object wm = Class.forName("android.view.WindowManagerGlobal").getMethod("getWindowManagerService").invoke(null);
                Class<?> windowManager = Class.forName("android.view.IWindowManager");
                windowManager.getMethod("setDisplayImePolicy", int.class, int.class).invoke(wm, display, 2);
                require((Integer) windowManager.getMethod("getDisplayImePolicy", int.class).invoke(wm, display) == 2,
                    "Virtual display IME isolation unavailable");
                uniqueId = displayUniqueId(); validate();
            } catch (Exception error) { if (created != null) created.release(); reader.close(); throw error; }
        }
        ImageReader createReader(PhoneUseDisplay config) {
            ImageReader next = ImageReader.newInstance(config.width, config.height, PixelFormat.RGBA_8888, 3);
            next.setOnImageAvailableListener(r -> {
                try (Image image = r.acquireLatestImage()) {
                    if (image == null || r != reader) return;
                    Image.Plane plane = image.getPlanes()[0];
                    Bitmap padded = Bitmap.createBitmap(plane.getRowStride() / plane.getPixelStride(), config.height, Bitmap.Config.ARGB_8888);
                    padded.copyPixelsFromBuffer(plane.getBuffer());
                    Bitmap frame = Bitmap.createBitmap(padded, 0, 0, config.width, config.height); if (frame != padded) padded.recycle();
                    synchronized (this) {
                        if (closed || r != reader) frame.recycle();
                        else { if (latest != null) latest.recycle(); latest = frame; notifyAll(); }
                    }
                } catch (Exception ignored) {}
            }, new Handler(captureThread.getLooper()));
            return next;
        }
        void awaitGeometry() throws Exception {
            long until = SystemClock.uptimeMillis() + 3000;
            do {
                android.util.DisplayMetrics metrics = new android.util.DisplayMetrics(); vd.getDisplay().getRealMetrics(metrics);
                if (metrics.widthPixels == width && metrics.heightPixels == height && metrics.densityDpi == density) {validate();return;}
                Thread.sleep(30);
            } while (SystemClock.uptimeMillis() < until);
            throw new IllegalStateException("Display adjustment unconfirmed");
        }
        void resize(PhoneUseDisplay config) throws Exception {
            validate();
            if (width == config.width && height == config.height && density == config.density) return;
            ImageReader previous = reader, replacement = createReader(config);
            PhoneUseDisplay old = new PhoneUseDisplay(width, height, density);
            revoke();
            synchronized (this) {
                reader = replacement; width = config.width; height = config.height; density = config.density;
                if (latest != null) latest.recycle(); latest = null;
            }
            try {
                vd.resize(width, height, density); vd.setSurface(replacement.getSurface()); awaitGeometry(); previous.close();
            } catch (Exception error) {
                synchronized (this) {
                    reader = previous; width = old.width; height = old.height; density = old.density;
                    if (latest != null) latest.recycle(); latest = null;
                }
                try {vd.resize(width, height, density);vd.setSurface(previous.getSurface());awaitGeometry();}
                catch (Exception rollback) {broken = true;}
                replacement.close(); throw error;
            }
        }
        void validate() {
            require(!closed && !broken, "Display adjustment lost confirmation; reconnect PhoneUse");
            require(vd.getDisplay().isValid() && vd.getDisplay().getDisplayId() == display && uniqueId.equals(displayUniqueId()), "Virtual display lease lost; reconnect and rediscover");
            android.util.DisplayMetrics metrics = new android.util.DisplayMetrics(); vd.getDisplay().getRealMetrics(metrics);
            require(metrics.widthPixels == width && metrics.heightPixels == height && metrics.densityDpi == density,
                "Virtual display dimensions or density changed; reconnect and rediscover");
            int flags = vd.getDisplay().getFlags();
            require((flags & 0x1800) == 0x1800, "Required display focus isolation unavailable");
        }
        String displayUniqueId() {
            try { return (String) android.view.Display.class.getMethod("getUniqueId").invoke(vd.getDisplay()); }
            catch (Exception error) { throw new IllegalStateException("Cannot verify display identity"); }
        }
        boolean hasTasks() { try { return !taskIds(display).isEmpty(); } catch (Exception error) { return true; } }
        void revoke() { windows.clear(); }
        List<AccessibilityWindowInfo> actual() {
            validate(); List<AccessibilityWindowInfo> found = automation.getWindowsOnAllDisplays().get(display);
            return found == null ? Collections.emptyList() : found;
        }
        void discover() throws Exception {
            Set<String> live = new HashSet<>();
            for (AccessibilityWindowInfo actual : actual()) {
                AccessibilityNodeInfo root = actual.getRoot();
                if (root == null || root.getPackageName() == null || ownPackage(root.getPackageName().toString())) continue;
                int task = (Integer) AccessibilityWindowInfo.class.getMethod("getTaskId").invoke(actual);
                if (task < 0 || !taskIds(display).contains(task)) continue;
                Rect bounds = new Rect(); actual.getBoundsInScreen(bounds);
                if (!new Rect(0, 0, width, height).contains(bounds) || bounds.isEmpty()) continue;
                Window found = null;
                for (Window w : windows.values()) if (w.platform == actual.getId() && w.task == task && w.root.equals(root)) found = w;
                if (found == null) { found = new Window(); found.platform = actual.getId(); found.task = task; found.root = root; windows.put(found.id, found); }
                if (found.bounds != null && !found.bounds.equals(bounds)) { found.state = null; found.receiver = null; }
                found.bounds = bounds; found.pkg = root.getPackageName().toString(); found.title = actual.getTitle() == null ? found.pkg : actual.getTitle().toString(); live.add(found.id);
            }
            windows.keySet().retainAll(live);
        }
        AccessibilityWindowInfo current(Window window) throws Exception {
            require(taskIds(display).contains(window.task), "Task left the owned display");
            for (AccessibilityWindowInfo actual : actual()) if (actual.getId() == window.platform) {
                AccessibilityNodeInfo root = actual.getRoot(); Rect bounds = new Rect(); actual.getBoundsInScreen(bounds);
                require(root != null && window.root.equals(root) && window.pkg.contentEquals(root.getPackageName())
                    && (Integer) AccessibilityWindowInfo.class.getMethod("getTaskId").invoke(actual) == window.task, "Window identity changed; rediscover");
                require(bounds.equals(window.bounds), "Window geometry changed; list_windows and observe again");
                return actual;
            }
            throw new IllegalStateException("Window no longer exists; rediscover");
        }
        void unobscured(Window window) throws Exception {
            AccessibilityWindowInfo target = current(window);
            for (AccessibilityWindowInfo other : actual()) {
                Rect bounds = new Rect(); other.getBoundsInScreen(bounds);
                require(other.getId() == target.getId() || other.getLayer() <= target.getLayer() || !Rect.intersects(window.bounds, bounds), "Window obscured; select its dialog or observe the top window");
            }
        }
        byte[] preview() throws Exception {
            validate();
            byte[] bytes;
            synchronized (this) {
                if (latest == null) return null;
                ByteArrayOutputStream output = new ByteArrayOutputStream();
                latest.compress(Bitmap.CompressFormat.PNG, 100, output);
                bytes = output.toByteArray();
            }
            validate();
            return bytes;
        }
        ComputerReply observe(Window window, Rect region, int scale, String status) throws Exception {
            window.state = null; unobscured(window);
            if (region == null) region = new Rect(0, 0, window.bounds.width(), window.bounds.height());
            require(!region.isEmpty() && new Rect(0, 0, window.bounds.width(), window.bounds.height()).contains(region)
                && scale >= 1 && scale <= 4 && (long) region.width() * region.height() * scale * scale <= 64_000_000, "Invalid crop or image size");
            byte[] image;
            synchronized (this) {
                long deadline = SystemClock.uptimeMillis() + 3000;
                while (latest == null && SystemClock.uptimeMillis() < deadline) wait(100);
                require(latest != null, "No rendered frame available");
                Bitmap crop = Bitmap.createBitmap(latest, window.bounds.left + region.left, window.bounds.top + region.top, region.width(), region.height());
                Bitmap output = scale == 1 ? crop : Bitmap.createScaledBitmap(crop, region.width() * scale, region.height() * scale, false);
                ByteArrayOutputStream bytes = new ByteArrayOutputStream(); output.compress(Bitmap.CompressFormat.PNG, 100, bytes); image = bytes.toByteArray();
                if (output != crop) output.recycle(); if (crop != latest) crop.recycle();
            }
            unobscured(window); window.region = region; window.scale = scale; window.state = id();
            JSONObject receiver = null;
            if (window.receiver != null && window.receiver.refresh()) receiver = object("kind", "editable_control", "text", window.receiver.getText() == null ? "" : window.receiver.getText().toString());
            else window.receiver = null;
            JSONArray actions = new JSONArray(Arrays.asList("click_left", "scroll_vertical", "drag_touch", "Back", "Enter", "Escape", "Backspace", "Delete", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "release_window", "close_window"));
            if (window.receiver != null && window.receiver.getActionList().contains(AccessibilityNodeInfo.AccessibilityAction.ACTION_SET_TEXT)) actions.put("set_text");
            return reply(object("status", status, "observation", object("window_id", window.id, "state_id", window.state, "title", window.title,
                "width", region.width() * scale, "height", region.height() * scale, "window_width", window.bounds.width(), "window_height", window.bounds.height(),
                "display", object("width", width, "height", height, "density", density),
                "view", object("region", rect(region), "scale", scale), "input_target", receiver, "supported_actions", actions,
                "screenshot", null, "screenshot_url", null)), image, null, hasTasks());
        }
        @Override public void close() {
            synchronized (this) { if (closed) return; closed = true; if (latest != null) latest.recycle(); latest = null; }
            windows.clear(); vd.release(); reader.close();
        }
    }
}
