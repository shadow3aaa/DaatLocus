package io.daat_locus.app

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import android.net.wifi.WifiManager
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.os.Handler
import android.os.Looper
import androidx.annotation.Keep
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import kotlin.math.max

class MainActivity : TauriActivity() {
  private var multicast: WifiManager.MulticastLock? = null
  private val connectivity by lazy { getSystemService(CONNECTIVITY_SERVICE) as ConnectivityManager }
  private val networkHandler = Handler(Looper.getMainLooper())
  @Keep
  private external fun nativeNetworkChanged()
  private val networkChanged = Runnable { nativeNetworkChanged() }
  private fun queueNetworkChange() {
    networkHandler.removeCallbacks(networkChanged)
    networkHandler.postDelayed(networkChanged, 300)
  }
  private val networkCallback = object : ConnectivityManager.NetworkCallback() {
    override fun onAvailable(network: Network) = queueNetworkChange()
    override fun onLost(network: Network) = queueNetworkChange()
    override fun onLinkPropertiesChanged(network: Network, properties: LinkProperties) = queueNetworkChange()
  }
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    connectivity.registerDefaultNetworkCallback(networkCallback)
    ViewCompat.setOnApplyWindowInsetsListener(findViewById(android.R.id.content)) { view, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime())
      view.setPadding(bars.left, bars.top, bars.right, max(bars.bottom, keyboard.bottom))
      // The native container already applies system and keyboard insets. Do not
      // forward them to WebView, which would reserve the same safe area again.
      WindowInsetsCompat.CONSUMED
    }
  }
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        webView.evaluateJavascript("""
          (() => {
            if (document.querySelector('[data-slot="dialog-content"][data-state="open"]')) {
              document.dispatchEvent(new KeyboardEvent('keydown', {key:'Escape', bubbles:true}));
              return true;
            }
            if (location.pathname !== '/' && history.length > 1) { history.back(); return true; }
            return false;
          })()
        """.trimIndent()) { handled -> if (handled != "true") moveTaskToBack(true) }
      }
    })
  }
  override fun onStart() {
    super.onStart()
    val wifi = applicationContext.getSystemService(WIFI_SERVICE) as WifiManager
    multicast = wifi.createMulticastLock("DaatLocus.discovery").apply {
      setReferenceCounted(false)
      acquire()
    }
  }
  override fun onStop() {
    multicast?.let { if (it.isHeld) it.release() }
    multicast = null
    super.onStop()
  }
  override fun onDestroy() {
    connectivity.unregisterNetworkCallback(networkCallback)
    networkHandler.removeCallbacks(networkChanged)
    super.onDestroy()
  }
}
