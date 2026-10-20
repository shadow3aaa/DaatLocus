package io.shadow3aaa.daatlocus

import android.content.Context
import android.os.Binder
import android.os.IBinder
import android.os.Parcel
import android.os.ParcelFileDescriptor
import android.os.Parcelable
import androidx.annotation.Keep
import be.mygod.librootkotlinx.RootCommand
import be.mygod.librootkotlinx.systemContext
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.parcelize.Parcelize
import org.json.JSONObject
import java.util.concurrent.Executors

/** Binary images travel through an FD, never Binder-sized base64 JSON. */
@Keep
@Parcelize
data class ComputerReply(val json: String, val image: ParcelFileDescriptor? = null) : Parcelable

@Keep
@Parcelize
class ComputerRootCommand(private val request: String) : RootCommand<ComputerReply> {
    override suspend fun execute(): ComputerReply = withContext(Dispatchers.IO) {
        PhoneUseHost.handle(systemContext, request)
    }
}

/** Only DL's native process may invoke this Shizuku UserService. */
@Keep
class PhoneUseService(context: Context) : Binder() {
    private val context = context
    private val owner = context.packageManager.getApplicationInfo(BuildConfig.APPLICATION_ID, 0).uid
    override fun onTransact(code: Int, data: Parcel, reply: Parcel?, flags: Int): Boolean {
        val caller = Binder.getCallingUid()
        if (code == 16777115) {
            // Shizuku itself sends the documented UserService destroy transaction.
            if (caller != owner && caller != 0 && caller != 2000) throw SecurityException("PhoneUse caller rejected")
            PhoneUseHost.close()
            reply?.writeNoException()
            kotlin.system.exitProcess(0)
        }
        if (caller != owner) throw SecurityException("PhoneUse caller rejected")
        if (code != FIRST_CALL_TRANSACTION || reply == null) return super.onTransact(code, data, reply, flags)
        data.enforceInterface(DESCRIPTOR)
        val request = requireNotNull(data.readString())
        require(request.length <= 65536) { "PhoneUse request too large" }
        val result = PhoneUseHost.handle(context, request)
        reply.writeNoException()
        reply.writeParcelable(result, Parcelable.PARCELABLE_WRITE_RETURN_VALUE)
        result.image?.close()
        return true
    }
    companion object {
        const val DESCRIPTOR = "io.shadow3aaa.daatlocus.PhoneUse.v1"
        fun call(service: IBinder, request: String): ComputerReply {
            val data = Parcel.obtain()
            val reply = Parcel.obtain()
            try {
                data.writeInterfaceToken(DESCRIPTOR)
                data.writeString(request)
                check(service.transact(FIRST_CALL_TRANSACTION, data, reply, 0)) { "PhoneUse transaction rejected" }
                reply.readException()
                @Suppress("DEPRECATION")
                return requireNotNull(reply.readParcelable(ComputerReply::class.java.classLoader))
            } finally { data.recycle(); reply.recycle() }
        }
    }
}

internal object ComputerImages {
    private val writers = Executors.newCachedThreadPool { task -> Thread(task, "dl-image-pipe").apply { isDaemon = true } }
    fun pipe(bytes: ByteArray): ParcelFileDescriptor {
        val pipe = ParcelFileDescriptor.createPipe()
        writers.execute { runCatching { ParcelFileDescriptor.AutoCloseOutputStream(pipe[1]).use { it.write(bytes) } } }
        return pipe[0]
    }
    fun error(message: String, status: String = "not_started") = ComputerReply(JSONObject()
        .put("summary", "PhoneUse unavailable")
        .put("failure", message)
        .put("payload", JSONObject().put("status", status).put("error", message)).toString())
}
