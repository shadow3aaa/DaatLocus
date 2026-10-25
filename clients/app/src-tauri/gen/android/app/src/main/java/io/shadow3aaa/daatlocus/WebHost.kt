package io.shadow3aaa.daatlocus

import android.app.Dialog
import android.net.http.SslError
import android.webkit.*
import android.view.ViewGroup
import androidx.annotation.Keep
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import androidx.lifecycle.Lifecycle
import org.json.JSONObject
import org.json.JSONTokener
import java.lang.ref.WeakReference
import java.net.URI
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** Only native JNI invokes this class. No JavascriptInterface is installed. */
@Keep
object WebHost {
  internal var activity = WeakReference<MainActivity>(null)
  @Volatile private var serviceOrigin = ""
  internal data class Page(val view: WebView, val dialog: Dialog)
  private data class Pending(val id: String, val result: CompletableFuture<String>)
  internal val pages = mutableMapOf<String, Page>()
  private val pending = mutableMapOf<String, Pending>()
  fun initialize(value: MainActivity) { activity = WeakReference(value) }
  internal var clientView=WeakReference<WebView>(null)
  @JvmStatic fun browserControl(value:String):String {
    val owner=activity.get() ?: return JSONObject().put("error","Native client unavailable").toString()
    val result=CompletableFuture<String>()
    owner.runOnUiThread { try{BrowserUse.controlAsync(owner,JSONObject(value)){reply->result.complete(reply.toString())}}catch(error:Exception){result.complete(JSONObject().put("error",error.message ?: "Browser unavailable").toString())} }
    return try{result.get(120,TimeUnit.SECONDS)}catch(_:Exception){JSONObject().put("error","Browser operation timed out").toString()}
  }
  @JvmStatic fun setServiceOrigin(value: String) { serviceOrigin = value.trimEnd('/') }
  internal fun allowed(value: String): Boolean = try {
    val uri = URI(value)
    val port = if (uri.port < 0) "" else ":${uri.port}"
    uri.scheme in listOf("https", "http") && !uri.host.isNullOrEmpty() && uri.rawUserInfo == null &&
      "${uri.scheme}://${uri.host}$port" != serviceOrigin
  } catch (_: Exception) { false }
  private fun reply(id: String, capture: JSONObject? = null, error: String? = null): String =
    JSONObject().put("id", id).put("capture", capture ?: JSONObject.NULL).put("error", error ?: JSONObject.NULL).toString()
  private fun fail(profile: String, error: String) {
    pending.remove(profile)?.let { it.result.complete(reply(it.id, error = error)) }
  }
  internal fun close(profile: String) {
    fail(profile, "Web page closed")
    pages.remove(profile)?.let {
      it.view.stopLoading()
      if(profile.length==64){it.dialog.setOnDismissListener(null);it.dialog.dismiss()} else { (it.view.parent as? ViewGroup)?.removeView(it.view);it.dialog.dismiss();BrowserUse.remove(profile) }
      BrowserUse.forget(it.view)
      it.view.destroy()
    }
  }
  fun shutdown() { pages.keys.toList().forEach(::close); BrowserUse.shutdown();activity.clear() }
  internal fun page(profile: String, key: String): Page = pages[key] ?: run {
    val owner=activity.get() ?: throw IllegalStateException("Native Web Activity unavailable")

          val view = WebView(owner)
          if (!WebViewFeature.isFeatureSupported(WebViewFeature.MULTI_PROFILE)) {
            view.destroy(); throw IllegalStateException("This Android WebView cannot isolate browser profiles; update the WebView provider")
          }
          WebViewCompat.setProfile(view, "dl-web-$profile")
          view.settings.javaScriptEnabled = true
          view.settings.domStorageEnabled = true
          view.settings.allowFileAccess = false
          view.settings.allowContentAccess = false
          view.settings.mixedContentMode = WebSettings.MIXED_CONTENT_NEVER_ALLOW
          view.settings.javaScriptCanOpenWindowsAutomatically = false
          view.settings.setSupportMultipleWindows(key!=profile)
          view.setDownloadListener { url,agent,disposition,mime,_ -> if(key!=profile)BrowserUse.download(owner,view,url,agent,disposition,mime) }
          view.webViewClient=object:WebViewClient(){
            override fun shouldOverrideUrlLoading(view:WebView,request:WebResourceRequest)=request.url.toString()!="about:blank" && !allowed(request.url.toString())
            override fun onPageStarted(view:WebView,url:String,favicon:android.graphics.Bitmap?){if(key!=profile)BrowserUse.changed()}
            override fun onPageFinished(view:WebView,url:String){if(key!=profile)BrowserUse.finished(owner,key,view,url)}
            override fun onReceivedSslError(view:WebView,handler:SslErrorHandler,error:SslError){handler.cancel()}
          }
          view.webChromeClient = object : WebChromeClient() {
            override fun onReceivedTitle(view:WebView,title:String){if(key!=profile)BrowserUse.changed()}
            override fun onProgressChanged(view:WebView,progress:Int){if(key!=profile)BrowserUse.changed()}
            override fun onReceivedIcon(view:WebView,icon:android.graphics.Bitmap){if(key!=profile)BrowserUse.receivedIcon(owner,key,view,icon)}
            override fun onCreateWindow(view:WebView,isDialog:Boolean,isUserGesture:Boolean,resultMsg:android.os.Message):Boolean{
              if(key==profile || !isUserGesture)return false
              val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";val target=page(profile,next)
              (resultMsg.obj as WebView.WebViewTransport).webView=target.view;resultMsg.sendToTarget();return true
            }
            override fun onPermissionRequest(request: PermissionRequest) { request.deny() }
          }
          if (key != profile) return BrowserUse.mount(owner, key, view).let { Page(view,it).also { page -> pages[key]=page } }
          val dialog = Dialog(owner)
          dialog.setTitle("Web")
          dialog.setContentView(view)
          dialog.setOnDismissListener { close(key) }
          // Document capture runs independently of the visible shared browser.
          // Give the unattached WebView a viewport without presenting a dialog.
          val size = owner.resources.displayMetrics
          view.measure(android.view.View.MeasureSpec.makeMeasureSpec(size.widthPixels, android.view.View.MeasureSpec.EXACTLY), android.view.View.MeasureSpec.makeMeasureSpec(size.heightPixels, android.view.View.MeasureSpec.EXACTLY))
          view.layout(0, 0, size.widthPixels, size.heightPixels)
          Page(view, dialog).also { pages[key] = it }

  }
  @JvmStatic fun request(value: String, script: String, metrics: String, cursor:String): String {
    val request = JSONObject(value)
    val id = request.getString("id")
    val profile = request.getString("profile")
    if (!id.matches(Regex("[0-9a-f]{32}")) || !profile.matches(Regex("[0-9a-f]{64}"))) return reply(id, error = "Invalid Web request")
    val owner = activity.get() ?: return reply(id, error = "Native Web Activity unavailable")
    if (request.getJSONObject("operation").getString("operation") == "browser") return BrowserUse.request(owner,profile,id,request.getJSONObject("operation").getJSONObject("command"),metrics,cursor)
    val result = CompletableFuture<String>()
    owner.runOnUiThread {
      try {
        if (request.getJSONObject("operation").getString("operation") == "close") {
          close(profile); result.complete(reply(id)); return@runOnUiThread
        }
        if (!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED)) throw IllegalStateException("Open the native client to load a rendered Web page")
        val url = request.getJSONObject("operation").getString("url")
        if (!allowed(url)) throw IllegalArgumentException("Web navigation target is not allowed")
        fail(profile, "Web request superseded")
        pending[profile] = Pending(id, result)
        val page = page(profile,profile)
        // Each request owns its completion closure; a stale navigation cannot
        // complete a later request or replace its immutable document reference.
        page.view.webViewClient = object : WebViewClient() {
          override fun shouldOverrideUrlLoading(view: WebView, request: WebResourceRequest) = !allowed(request.url.toString())
          override fun onReceivedSslError(view: WebView, handler: SslErrorHandler, error: SslError) { handler.cancel(); if (pending[profile]?.id == id) fail(profile, "Web certificate validation failed") }
          override fun onReceivedError(view: WebView, request: WebResourceRequest, error: WebResourceError) { if (request.isForMainFrame && pending[profile]?.id == id) fail(profile, "Web navigation failed: ${error.errorCode}") }
          override fun onPageFinished(view: WebView, url: String) {
            if (pending[profile]?.id != id || !allowed(url)) return
            view.evaluateJavascript(script) { encoded ->
              if (pending[profile]?.id != id) return@evaluateJavascript
              try {
                if (encoded.length > 24 * 1024 * 1024) throw IllegalStateException("Web capture exceeds its limit")
                val decoded = JSONTokener(encoded).nextValue()
                val captured = if (decoded is String) JSONObject(decoded) else decoded as JSONObject
                val response = if (captured.has("error")) reply(id, error = captured.getString("error")) else reply(id, captured)
                pending.remove(profile)?.result?.complete(response)
              } catch (_: Exception) { fail(profile, "Web document capture failed") }
            }
          }
        }
        page.view.loadUrl(url)
      } catch (error: Exception) { fail(profile, error.message ?: "Web host unavailable"); result.complete(reply(id, error = error.message ?: "Web host unavailable")) }
    }
    return try { result.get(27, TimeUnit.SECONDS) } catch (_: Exception) {
      owner.runOnUiThread { if (pending[profile]?.id == id) { fail(profile, "Web request timed out; navigation may have occurred"); pages[profile]?.view?.stopLoading() } }
      reply(id, error = "Web loading or capture timed out; do not replay automatically")
    }
  }
}
