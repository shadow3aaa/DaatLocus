package io.shadow3aaa.daatlocus

import android.content.ComponentName
import android.content.Context
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import androidx.annotation.Keep
import be.mygod.librootkotlinx.RootServer
import be.mygod.librootkotlinx.RootSession
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withContext
import org.json.JSONObject
import rikka.shizuku.Shizuku
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/** Owner-only configuration is reached through a narrow, origin-checked native command.
 * Saved Owner preferences restore transports independently of model tool calls. */
@Keep
object PhoneUseController {
    private lateinit var context: Context
    private val mutex = Mutex()
    private val main = Handler(Looper.getMainLooper())
    private val lifecycle = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private val revision = AtomicLong()
    @Volatile private var enabled = false
    @Volatile private var method = "shizuku"
    @Volatile private var state = "disabled"
    @Volatile private var uid = -1
    @Volatile private var display = PhoneUseDisplay.DEFAULT
    @Volatile private var binder: IBinder? = null
    private var rootSession: RootSession? = null
    @Volatile private var root: RootServer? = null
    private var binding: Shizuku.UserServiceArgs? = null
    private var connection: ServiceConnection? = null
    private var permission: CompletableDeferred<Boolean>? = null
    private val images = ConcurrentHashMap<String, ByteArray>()
    private val resources = ConcurrentHashMap.newKeySet<String>()
    private val permissionListener = Shizuku.OnRequestPermissionResultListener { code, result ->
        if (code == 2841) permission?.complete(result == PackageManager.PERMISSION_GRANTED)
    }
    @Synchronized fun initialize(value: Context) {
        if (::context.isInitialized) return
        context = value.applicationContext
        val prefs = context.getSharedPreferences("phone_use", Context.MODE_PRIVATE)
        enabled = prefs.getBoolean("enabled", false)
        method = prefs.getString("method", "shizuku") ?: "shizuku"
        display = runCatching { PhoneUseDisplay(prefs.getInt("width", 1280), prefs.getInt("height", 720), prefs.getInt("density", 160)) }
            .getOrDefault(PhoneUseDisplay.DEFAULT)
        state = if (enabled) "disconnected" else "disabled"
        Shizuku.addRequestPermissionResultListener(permissionListener)
        Shizuku.addBinderDeadListener { if (method == "shizuku") disconnected() }
        Shizuku.addBinderReceivedListenerSticky { if (method == "shizuku") resume() }
        resume()
    }
    private fun disconnected() {
        state = if (enabled) "disconnected" else "disabled"
        binder = null
        uid = -1
        resources.clear()
        images.clear()
    }
    @JvmStatic fun status(): String {
        if (state == "ready" && !transportAlive()) {
            disconnected()
            resume()
        }
        return JSONObject().put("enabled", enabled).put("method", method)
            .put("state", state).put("uid", if (uid < 0) JSONObject.NULL else uid).put("display", display.json()).toString()
    }

    /** Recreate the process-local connection, retaining the saved Owner choice.
     * Shizuku recovery only uses an existing grant; prompts remain explicit configuration. */
    @JvmStatic fun resume() {
        if (!::context.isInitialized || !enabled) return
        if (state == "ready" && !transportAlive()) disconnected()
        if (state == "ready" || state == "connecting" || state == "unsupported") return
        val expected = revision.get()
        lifecycle.launch {
            mutex.withLock {
                if (!enabled || revision.get() != expected || state == "ready") return@withLock
                disconnect()
                connect(false, expected)
            }
        }
    }
    private fun transportAlive() = if (method == "root") root?.active == true else binder?.isBinderAlive == true

    @JvmStatic fun configureDisplay(request: String): String = runBlocking(Dispatchers.IO) {
        mutex.withLock {
            val patch = JSONObject(request)
            require(patch.keys().asSequence().all { it in setOf("width", "height", "density") })
            fun value(key: String, current: Int) = if (patch.isNull(key)) current else patch.getInt(key)
            val next = PhoneUseDisplay(value("width", display.width), value("height", display.height), value("density", display.density))
            check(!enabled || state == "ready") { "Reconnect PhoneUse before applying display settings" }
            suspend fun resize(config: PhoneUseDisplay) {
                val reply = send(JSONObject().put("op", "configure_display").toString(), config)
                reply.image?.close()
                val result = JSONObject(reply.json)
                if (result.getJSONObject("payload").optBoolean("reconnect_required")) state = "unavailable"
                check(result.isNull("failure")) { "Display adjustment failed; settings were not saved" }
            }
            if (enabled) resize(next)
            val persisted = context.getSharedPreferences("phone_use", Context.MODE_PRIVATE).edit()
                .putInt("width", next.width).putInt("height", next.height).putInt("density", next.density).commit()
            if (!persisted) {
                if (enabled) runCatching { resize(display) }.onFailure { state = "unavailable" }
                error("Cannot save display preference")
            }
            display = next
            status()
        }
    }

    @JvmStatic fun scope(): String {
        check(enabled && state == "ready") { "Enable or reconnect PhoneUse in this device's Settings" }
        return JSONObject().put("method", method).put("uid", uid).toString()
    }
    @JvmStatic fun hasResources(session: String) = resources.contains(session)
    @JvmStatic fun image(request: String): ByteArray = images.remove(request) ?: byteArrayOf()

    @JvmStatic fun configure(value: Boolean, selected: String): String = runBlocking(Dispatchers.IO) {
        require(selected == "shizuku" || selected == "root")
        // Admission closes before waiting for any in-flight call. Its outcome is never replayed.
        val expected = revision.incrementAndGet()
        enabled = false
        mutex.withLock {
            if (revision.get() != expected) return@withLock status()
            disconnect()
            check(context.getSharedPreferences("phone_use", Context.MODE_PRIVATE).edit()
                .putBoolean("enabled", value).putString("method", selected).commit()) { "Cannot save PhoneUse preference" }
            method = selected
            enabled = value
            if (!enabled) { state = "disabled"; return@withLock status() }
            connect(true, expected)
            status()
        }
    }
    private suspend fun connect(requestPermission: Boolean, expected: Long) {
        if (Build.VERSION.SDK_INT < 35) { state = "unsupported"; return }
        if (method == "shizuku" && !Shizuku.pingBinder()) { state = "disconnected"; return }
        state = "connecting"
        try {
            withTimeout(60000) {
                if (method == "shizuku") connectShizuku(requestPermission, expected) else {
                    rootSession = object : RootSession() { override val context get() = PhoneUseController.context }
                    root = rootSession!!.acquire()
                }
                check(enabled && revision.get() == expected) { "PhoneUse configuration changed" }
                val probe = send(JSONObject().put("op", "probe").toString())
                probe.image?.close()
                val result = JSONObject(probe.json)
                check(result.isNull("failure")) { "PhoneUse capability check failed" }
                val actualUid = result.getJSONObject("payload").getInt("uid")
                check(actualUid == if (method == "root") 0 else 2000) { "Unexpected PhoneUse identity" }
                check(enabled && revision.get() == expected) { "PhoneUse configuration changed" }
                uid = actualUid
                state = "ready"
            }
        } catch (_: Throwable) {
            withContext(NonCancellable) { disconnect() }
            state = if (enabled) "unavailable" else "disabled"
        }
    }
    private suspend fun connectShizuku(requestPermission: Boolean, expected: Long) {
        check(Shizuku.pingBinder()) { "Start Shizuku first" }
        if (Shizuku.checkSelfPermission() != PackageManager.PERMISSION_GRANTED) {
            check(requestPermission) { "Shizuku permission is no longer granted" }
            val granted = CompletableDeferred<Boolean>()
            permission = granted
            try {
                main.post {
                    if (enabled && revision.get() == expected) Shizuku.requestPermission(2841)
                    else granted.complete(false)
                }
                check(granted.await()) { "Shizuku permission denied" }
            } finally { permission = null }
        }
        val connected = CompletableDeferred<IBinder>()
        val args = Shizuku.UserServiceArgs(ComponentName(context, PhoneUseService::class.java))
            .daemon(false).processNameSuffix("phone_use").tag("phone_use_v1").version(BuildConfig.VERSION_CODE)
        val serviceConnection = object : ServiceConnection {
            override fun onServiceConnected(name: ComponentName, service: IBinder) {
                if (!enabled || revision.get() != expected || binding !== args || connection !== this) return
                service.linkToDeath({
                    if (binder === service) { disconnected(); resume() }
                }, 0)
                binder = service
                connected.complete(service)
            }
            override fun onServiceDisconnected(name: ComponentName) {
                if (binding === args && connection === this) { disconnected(); resume() }
            }
        }
        binding = args; connection = serviceConnection
        main.post {
            if (!enabled || revision.get() != expected || binding !== args || connection !== serviceConnection) {
                connected.completeExceptionally(IllegalStateException("PhoneUse configuration changed"))
            } else runCatching { Shizuku.bindUserService(args, serviceConnection) }.onFailure { connected.completeExceptionally(it) }
        }
        connected.await()
    }
    private suspend fun send(request: String, config: PhoneUseDisplay = display): ComputerReply {
        val configured = JSONObject(request).put("display", config.json()).toString()
        return if (method == "shizuku") {
            PhoneUseService.call(checkNotNull(binder) { "Shizuku disconnected" }, configured)
        } else checkNotNull(root) { "Root disconnected" }.execute(ComputerRootCommand(configured), ComputerRootCommand::class.java.classLoader)
    }

    /** Read existing screens only; observation never reconnects or requests a grant. */
    @JvmStatic fun virtualDisplays(): String = runBlocking(Dispatchers.IO) {
        mutex.withLock {
            var current = when {
                !enabled -> "disabled"
                state == "unsupported" -> "unsupported"
                state == "connecting" -> "connecting"
                state == "ready" && transportAlive() -> "ready"
                else -> "unavailable"
            }
            var displays = org.json.JSONArray()
            if (current == "ready") {
                runCatching {
                    val reply = send(JSONObject().put("op", "preview_list").toString())
                    reply.image?.close()
                    val result = JSONObject(reply.json)
                    check(result.isNull("failure"))
                    displays = result.getJSONObject("payload").getJSONArray("displays")
                }.onFailure { current = "unavailable" }
            }
            JSONObject().put("supported", true).put("state", current).put("displays", displays).toString()
        }
    }
    @JvmStatic fun virtualDisplayImage(id: String): ByteArray = runBlocking(Dispatchers.IO) {
        mutex.withLock {
            if (!enabled || state != "ready" || !transportAlive()) return@withLock byteArrayOf()
            val reply = send(JSONObject().put("op", "preview_frame").put("display_id", id).toString())
            if (!JSONObject(reply.json).isNull("failure")) {
                reply.image?.close()
                error("Virtual display unavailable")
            }
            reply.image?.let { fd ->
                android.os.ParcelFileDescriptor.AutoCloseInputStream(fd).use { input ->
                    val bytes = input.readNBytes(32 * 1024 * 1024 + 1)
                    check(bytes.size <= 32 * 1024 * 1024) { "Screenshot exceeds limit" }
                    bytes
                }
            } ?: byteArrayOf()
        }
    }

    private suspend fun disconnect() {
        runCatching { if (binder != null || root != null) send(JSONObject().put("op", "stop").toString()).image?.close() }
        binding?.let { args -> connection?.let { conn -> runCatching { Shizuku.unbindUserService(args, conn, true) } } }
        root?.let { server -> runCatching { rootSession?.release(server) } }
        root = null
        runCatching { rootSession?.closeExisting() }; rootSession = null
        binding = null; connection = null
        disconnected()
    }
    @JvmStatic fun request(request: String): String = runBlocking(Dispatchers.IO) {
        mutex.withLock {
            val cmd = JSONObject(request)
            val id = cmd.getString("id")
            val session = cmd.getString("session")
            if (!enabled || state != "ready") {
                if (cmd.optString("op") == "detach") return@withLock ComputerReply(JSONObject().put("summary", "PhoneUse detached").put("payload", JSONObject()).toString()).json
                return@withLock ComputerImages.error("Enable or reconnect PhoneUse in Settings").json
            }
            try {
                val reply = send(request)
                val json = JSONObject(reply.json)
                reply.image?.let { fd ->
                    android.os.ParcelFileDescriptor.AutoCloseInputStream(fd).use { input ->
                        val bytes = input.readNBytes(8_000_001)
                        check(bytes.size <= 8_000_000) { "Screenshot exceeds limit" }
                        check(images.size < 8) { "Too many pending screenshots" }
                        images[id] = bytes
                    }
                }
                if (json.optBoolean("has_resources")) resources.add(session) else resources.remove(session)
                reply.json
            } catch (_: Throwable) {
                disconnected()
                resume()
                ComputerImages.error("PhoneUse connection lost; observe after reconnecting, do not replay", "outcome_unknown").json
            }
        }
    }
    @JvmStatic fun cancel(id: String) {
        images.remove(id)
        // Independent admission fence; this never retries the cancelled operation.
        val service = binder
        if (service != null) runCatching { PhoneUseService.call(service, JSONObject().put("op", "cancel").put("id", id).toString()).image?.close() }
        val server = root
        if (server != null) runBlocking(Dispatchers.IO) {
            runCatching { server.execute(ComputerRootCommand(JSONObject().put("op", "cancel").put("id", id).toString()), ComputerRootCommand::class.java.classLoader).image?.close() }
        }
    }
    /** Account work reset releases all Actor displays without changing the Owner's preference. */
    @JvmStatic fun reset(): String = runBlocking(Dispatchers.IO) {
        mutex.withLock {
            if (state == "ready") {
                val reply = send(JSONObject().put("op", "stop").toString())
                reply.image?.close()
                check(JSONObject(reply.json).isNull("failure")) { "PhoneUse reset failed" }
            }
            resources.clear(); images.clear()
            status()
        }
    }
}
