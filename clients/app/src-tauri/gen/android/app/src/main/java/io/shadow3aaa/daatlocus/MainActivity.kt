package io.shadow3aaa.daatlocus

import android.os.Bundle
import android.os.Build
import android.graphics.Color
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
import androidx.activity.SystemBarStyle
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.graphics.Insets

class MainActivity : TauriActivity() {
  private var browserScreenshot:Pair<ByteArray,(Boolean,String?)->Unit>?=null
  private val saveBrowserImage=registerForActivityResult(androidx.activity.result.contract.ActivityResultContracts.CreateDocument("image/png")){uri->
    val pending=browserScreenshot
    browserScreenshot=null
    if(pending!=null){
      if(uri==null)pending.second(false,null)
      else Thread{
        try{contentResolver.openOutputStream(uri)?.use{it.write(pending.first)} ?: throw java.io.IOException("File unavailable");runOnUiThread{pending.second(true,null)}}
        catch(_:Exception){runOnUiThread{pending.second(false,"Screenshot could not be saved")}}
      }.start()
    }
  }
  internal fun saveBrowserScreenshot(bytes:ByteArray,name:String,done:(Boolean,String?)->Unit){
    if(browserScreenshot!=null)throw IllegalStateException("A screenshot is already being saved")
    browserScreenshot=Pair(bytes,done)
    try{saveBrowserImage.launch(name)}catch(error:Exception){browserScreenshot=null;throw error}
  }
  private var multicast: WifiManager.MulticastLock? = null
  private val connectivity by lazy { getSystemService(CONNECTIVITY_SERVICE) as ConnectivityManager }
  private val networkHandler = Handler(Looper.getMainLooper())
  @Keep
  private external fun nativeNetworkChanged()
  @Keep
  private external fun nativeWindowInsetsChanged(top: Float, right: Float, bottom: Float, left: Float)
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
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.auto(Color.TRANSPARENT, Color.TRANSPARENT),
      // API 24/25 only supports light navigation icons; keep their contrast.
      navigationBarStyle = SystemBarStyle.auto(Color.TRANSPARENT,
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Color.TRANSPARENT else 0x801b1b1b.toInt()),
    )
    WebHost.initialize(this)
    super.onCreate(savedInstanceState)
    PhoneUseController.initialize(applicationContext)
    connectivity.registerDefaultNetworkCallback(networkCallback)
    ViewCompat.setOnApplyWindowInsetsListener(findViewById(android.R.id.content)) { view, insets ->
      // AndroidX's automatic style enables a separate three-button navigation
      // scrim. The WebView already draws this area and owns its safe spacing.
      // Insets are reapplied after navigation/configuration changes as well.
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
        window.isNavigationBarContrastEnforced = false
        window.isStatusBarContrastEnforced = false
      }
      val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
      val keyboard = insets.getInsets(WindowInsetsCompat.Type.ime())
      val imeVisible = insets.isVisible(WindowInsetsCompat.Type.ime())
      // Resize only for the keyboard, including on older WebViews. System bars
      // overlap the page background; individual web controls apply safe areas.
      view.setPadding(0, 0, 0, if (imeVisible) keyboard.bottom else 0)
      val density = resources.displayMetrics.density
      nativeWindowInsetsChanged(bars.top / density, bars.right / density,
        if (imeVisible) 0f else bars.bottom / density, bars.left / density)
      WindowInsetsCompat.Builder(insets)
        .setInsets(WindowInsetsCompat.Type.ime(), Insets.NONE)
        .build()
    }
    ViewCompat.requestApplyInsets(findViewById(android.R.id.content))
  }
  override fun onWebViewCreate(webView: WebView) {
    super.onWebViewCreate(webView)
    WebHost.clientView=java.lang.ref.WeakReference(webView)
    onBackPressedDispatcher.addCallback(this, object : OnBackPressedCallback(true) {
      override fun handleOnBackPressed() {
        webView.evaluateJavascript("""
          (() => {
            if (document.querySelector('[data-slot="context-menu-content"][data-state="open"], [data-slot="dialog-content"][data-state="open"]')) {
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
  override fun onResume() {
    super.onResume()
    PhoneUseController.resume()
  }
  override fun onStop() {
    multicast?.let { if (it.isHeld) it.release() }
    multicast = null
    super.onStop()
  }
  override fun onDestroy() {
    WebHost.shutdown()
    connectivity.unregisterNetworkCallback(networkCallback)
    networkHandler.removeCallbacks(networkChanged)
    super.onDestroy()
  }
}
