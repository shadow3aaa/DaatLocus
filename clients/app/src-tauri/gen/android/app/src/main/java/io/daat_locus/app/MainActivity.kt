package io.daat_locus.app

import android.os.Bundle
import android.webkit.WebView
import androidx.activity.OnBackPressedCallback
import android.net.wifi.WifiManager
import androidx.activity.enableEdgeToEdge
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import kotlin.math.max

class MainActivity : TauriActivity() {
  private var multicast: WifiManager.MulticastLock? = null
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    super.onCreate(savedInstanceState)
    ViewCompat.setOnApplyWindowInsetsListener(findViewById(android.R.id.content)) { view, insets ->
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime())
      view.setPadding(bars.left, bars.top, bars.right, max(bars.bottom, keyboard.bottom))
      insets
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
}
