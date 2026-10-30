package io.shadow3aaa.daatlocus

import android.app.Dialog
import android.view.View
import android.view.ViewGroup
import android.widget.*
import android.view.inputmethod.EditorInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.net.http.SslError
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Base64
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.webkit.*
import androidx.lifecycle.Lifecycle
import org.json.JSONArray
import org.json.JSONObject
import org.json.JSONTokener
import java.io.ByteArrayOutputStream
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** Native input only; this class is not a JavascriptInterface. All maps are UI-thread confined. */
internal object BrowserUse {
  /** Shell and page are hardware accelerated siblings; no Canvas clipping. */
  private class BrowserComposition(owner:MainActivity,val shell:WebView):FrameLayout(owner){
    var page:FrameLayout?=null
    var viewport=android.graphics.RectF()
    var inputRegions:List<android.graphics.RectF> = emptyList()
    private var gesture:View?=null
    private var rightInset=0f
    private var bottomInset=0f
    fun update(x:Int,y:Int,w:Int,h:Int,regions:List<android.graphics.RectF>){
      viewport.set(x.toFloat(),y.toFloat(),(x+w).toFloat(),(y+h).toFloat())
      rightInset=(shell.width-x-w).toFloat();bottomInset=(shell.height-y-h).toFloat()
      inputRegions=regions
      layoutPage()
    }
    private fun layoutPage(){page?.let{frame->
      val w=(width-viewport.left-rightInset).toInt().coerceAtLeast(1)
      val h=(height-viewport.top-bottomInset).toInt().coerceAtLeast(1)
      viewport.right=viewport.left+w;viewport.bottom=viewport.top+h
      val previous=frame.layoutParams
      if(previous==null||previous.width!=w||previous.height!=h)frame.layoutParams=LayoutParams(w,h)
      frame.x=viewport.left;frame.y=viewport.top
    }}
    override fun onSizeChanged(w:Int,h:Int,oldw:Int,oldh:Int){super.onSizeChanged(w,h,oldw,oldh);layoutPage()}
    private fun target(x:Float,y:Float):View=if(page?.visibility==View.VISIBLE&&viewport.contains(x,y)&&inputRegions.none{it.contains(x,y)})page!! else shell
    private fun dispatch(target:View,event:MotionEvent,generic:Boolean):Boolean{
      val local=MotionEvent.obtain(event)
      local.offsetLocation(-target.x,-target.y)
      return try{if(generic)target.dispatchGenericMotionEvent(local)else target.dispatchTouchEvent(local)}finally{local.recycle()}
    }
    override fun dispatchTouchEvent(event:MotionEvent):Boolean{
      if(event.actionMasked==MotionEvent.ACTION_DOWN)gesture=target(event.x,event.y)
      val target=gesture ?: shell
      val handled=dispatch(target,event,false)
      if(event.actionMasked==MotionEvent.ACTION_UP||event.actionMasked==MotionEvent.ACTION_CANCEL)gesture=null
      return handled
    }
    override fun dispatchGenericMotionEvent(event:MotionEvent):Boolean=dispatch(target(event.x,event.y),event,true)
  }
  private var composition:BrowserComposition?=null
  private fun composition(owner:MainActivity,client:WebView):BrowserComposition{
    composition?.let{return it}
    val parent=client.parent as? ViewGroup ?: throw IllegalStateException("Shell parent unavailable")
    val index=parent.indexOfChild(client);val params=client.layoutParams
    val root=BrowserComposition(owner,client)
    parent.removeView(client);parent.addView(root,index,params)
    client.setBackgroundColor(android.graphics.Color.TRANSPARENT)
    root.addView(client,FrameLayout.LayoutParams(-1,-1))
    composition=root
    return root
  }
  private val requestedUrls=mutableMapOf<String,String>()
  private val icons=mutableMapOf<String,String>()
  private val languageScripts=mutableMapOf<WebView,androidx.webkit.ScriptHandler>()
  fun changed(){WebHost.clientView.get()?.evaluateJavascript("window.dispatchEvent(new Event('dl-browser-changed'))",null)}
  private fun preferences(owner:MainActivity):JSONObject {
    val stored=owner.getSharedPreferences("browser-preferences",0)
    return JSONObject().put("search_engine",stored.getString("search_engine","google")).put("language",stored.getString("language","system")).put("default_zoom",stored.getString("default_zoom","1")!!.toDouble())
  }
  private fun patchPreferences(owner:MainActivity,patch:JSONObject):JSONObject {
    val next=preferences(owner)
    for(key in patch.keys()){if(key !in listOf("search_engine","language","default_zoom"))throw IllegalArgumentException("Unknown browser setting");next.put(key,patch.get(key))}
    if(next.getString("search_engine") !in listOf("google","bing","duckduckgo","baidu") || next.getString("language") !in listOf("system","zh-CN","en-US") || !next.getDouble("default_zoom").isFinite() || next.getDouble("default_zoom") !in .5..2.0)throw IllegalArgumentException("Invalid browser settings")
    if(!owner.getSharedPreferences("browser-preferences",0).edit().putString("search_engine",next.getString("search_engine")).putString("language",next.getString("language")).putString("default_zoom",next.getDouble("default_zoom").toString()).commit())throw IllegalStateException("Browser settings could not be saved")
    return next
  }
  fun configure(owner:MainActivity,view:WebView):String {
    val language=preferences(owner).getString("language").let{if(it=="system")java.util.Locale.getDefault().toLanguageTag() else it}
    if(androidx.webkit.WebViewFeature.isFeatureSupported(androidx.webkit.WebViewFeature.DOCUMENT_START_SCRIPT)){
      languageScripts.remove(view)?.remove()
      val locale=JSONObject.quote(language)
      val script="(()=>{const locale=$locale;for(const [key,value] of [['language',locale],['languages',[locale,locale.split('-')[0]]]])Object.defineProperty(Navigator.prototype,key,{get:()=>value,configurable:true})})()"
      languageScripts[view]=androidx.webkit.WebViewCompat.addDocumentStartJavaScript(view,script,setOf("*"))
    }
    return language
  }
  private fun navigate(owner:MainActivity,key:String,view:WebView,url:String){
    val language=configure(owner,view)
    if(url!="about:blank")requestedUrls[key]=url else requestedUrls.remove(key)
    changed();view.loadUrl(url,mapOf("Accept-Language" to "$language,${language.substringBefore('-')}"))
  }
  fun finished(owner:MainActivity,key:String,view:WebView,url:String){
    if(url!="about:blank")requestedUrls.remove(key)
    if(WebHost.allowed(url)&&!zooms.containsKey(key)){val zoom=preferences(owner).getDouble("default_zoom");view.zoomBy(zoom.toFloat());zooms[key]=zoom}
    visit(owner,url,view.title ?: "");changed()
  }
  fun receivedIcon(owner:MainActivity,key:String,view:WebView,bitmap:Bitmap){
    val small=Bitmap.createScaledBitmap(bitmap,32,32,true)
    val bytes=ByteArrayOutputStream().also{small.compress(Bitmap.CompressFormat.PNG,100,it)}.toByteArray()
    if(small!==bitmap)small.recycle()
    if(bytes.size>32768)return
    val icon="data:image/png;base64,"+Base64.encodeToString(bytes,Base64.NO_WRAP)
    icons[key]=icon
    val history=array(owner,"history");val origin=runCatching{java.net.URI(view.url ?: "").let{it.scheme+"://"+it.rawAuthority}}.getOrNull()
    if(origin!=null){for(index in 0 until history.length()){val entry=history.getJSONObject(index);if(runCatching{java.net.URI(entry.getString("url")).let{it.scheme+"://"+it.rawAuthority}}.getOrNull()==origin)entry.put("favicon",icon)};saveArray(owner,"history",history)}
    changed()
  }
  private var workspace:Dialog?=null
  private var content:FrameLayout?=null
  private var selected:String?=null
  // Transparent, non-focusable native roots keep each background page attached
  // without covering the Owner surface or sharing its keyboard recipient.
  private data class BackgroundPage(val dialog:Dialog,val root:FrameLayout)
  private val parking=mutableMapOf<String,BackgroundPage>()
  private var modelInputDepth=0
  private inline fun <T> modelInput(block:()->T):T {
    modelInputDepth++
    return try{block()}finally{modelInputDepth--}
  }
  private val tabOrder=mutableListOf<String>()
  private var profile=""
  private var embedded=false
  private var cursorScript=""
  fun shutdown(){workspace?.dismiss();content?.let{(it.parent as? ViewGroup)?.removeView(it)};workspace=null;content=null;selected=null;embedded=false;composition?.page=null;composition=null;parking.clear();jobs.clear();loaded.clear();frames.clear();tabOrder.clear()}
  private fun ensure(owner:MainActivity):Dialog {
    workspace?.let{return it}
    val dialog=Dialog(owner)
    content=FrameLayout(owner)
    dialog.setContentView(content!!)
    workspace=dialog
    return dialog
  }
  fun mount(owner:MainActivity,key:String,view:WebView):Dialog {
    profile=key.substringBefore('-')
    ensure(owner)
    configure(owner,view)
    val dialog=Dialog(owner)
    val root=FrameLayout(owner)
    root.importantForAccessibility=View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS
    dialog.requestWindowFeature(android.view.Window.FEATURE_NO_TITLE)
    dialog.setContentView(root)
    dialog.setCancelable(false)
    val background=BackgroundPage(dialog,root)
    parking[key]=background
    dialog.window?.let{window->
      window.addFlags(android.view.WindowManager.LayoutParams.FLAG_NOT_FOCUSABLE or android.view.WindowManager.LayoutParams.FLAG_NOT_TOUCHABLE or android.view.WindowManager.LayoutParams.FLAG_LAYOUT_NO_LIMITS)
      window.setBackgroundDrawable(android.graphics.drawable.ColorDrawable(android.graphics.Color.TRANSPARENT))
      window.setSoftInputMode(android.view.WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN or android.view.WindowManager.LayoutParams.SOFT_INPUT_ADJUST_NOTHING)
      window.attributes=window.attributes.apply{alpha=0f;dimAmount=0f}
    }
    root.addView(view,FrameLayout.LayoutParams(-1,-1))
    dialog.show()
    view.setOnKeyListener{_,code,event->
      if(modelInputDepth>0||event.action!=KeyEvent.ACTION_DOWN)return@setOnKeyListener false
      val letter=shortcut(code,event) ?: return@setOnKeyListener false
      WebHost.clientView.get()?.let{client->client.requestFocus();client.evaluateJavascript("window.dispatchEvent(new CustomEvent('dl-browser-shortcut',{detail:'$letter'}))",null)};true
    }
    if(key !in tabOrder)tabOrder.add(key)
    view.visibility=View.VISIBLE
    layoutParking(owner,background,view)
    return dialog
  }
  private fun layoutOffscreen(owner:MainActivity){
    if(embedded)return
    val client=WebHost.clientView.get()
    val width=(client?.width ?: owner.resources.displayMetrics.widthPixels).coerceAtLeast(1)
    val height=(client?.height ?: owner.resources.displayMetrics.heightPixels).coerceAtLeast(1)
    content?.measure(View.MeasureSpec.makeMeasureSpec(width,View.MeasureSpec.EXACTLY),View.MeasureSpec.makeMeasureSpec(height,View.MeasureSpec.EXACTLY))
    content?.layout(0,0,width,height)
  }
  fun forget(view:WebView){languageScripts.remove(view)?.remove()}
  private fun layoutParking(owner:MainActivity,background:BackgroundPage,view:WebView){
    val root=background.root
    val width=view.width.takeIf{it>0} ?: WebHost.clientView.get()?.width?.takeIf{it>0} ?: owner.resources.displayMetrics.widthPixels
    val height=view.height.takeIf{it>0} ?: WebHost.clientView.get()?.height?.takeIf{it>0} ?: owner.resources.displayMetrics.heightPixels
    background.dialog.window?.setLayout(width.coerceAtLeast(1),height.coerceAtLeast(1))
    root.measure(View.MeasureSpec.makeMeasureSpec(width.coerceAtLeast(1),View.MeasureSpec.EXACTLY),View.MeasureSpec.makeMeasureSpec(height.coerceAtLeast(1),View.MeasureSpec.EXACTLY))
    root.layout(0,0,width.coerceAtLeast(1),height.coerceAtLeast(1))
  }
  fun remove(key:String){
    frames.remove(key);requestedUrls.remove(key);icons.remove(key);zooms.remove(key);parking.remove(key)
    val index=tabOrder.indexOf(key);tabOrder.remove(key)
    if(selected==key){selected=null;tabOrder.getOrNull(index.coerceAtMost(tabOrder.lastIndex))?.let{select(it)}}
  }
  private fun select(key:String){
    val page=WebHost.pages[key] ?: return
    if(selected==key)return
    selected?.let{old->WebHost.pages[old]?.view?.let{view->parking[old]?.let{background->
      (view.parent as? ViewGroup)?.removeView(view);background.root.addView(view,FrameLayout.LayoutParams(-1,-1))
      WebHost.activity.get()?.let{owner->layoutParking(owner,background,view)}
    }}}
    (page.view.parent as? ViewGroup)?.removeView(page.view)
    content?.addView(page.view,FrameLayout.LayoutParams(-1,-1))
    selected=key
    page.view.visibility=View.VISIBLE
    WebHost.activity.get()?.let{layoutOffscreen(it)}
  }
  fun controlAsync(owner:MainActivity,command:JSONObject,done:(JSONObject)->Unit){
    fun fail(message:String){done(JSONObject().put("error",message))}
    try{
      val action=command.getString("action")
      if(action=="list"){
        val reply=control(owner,command)
        val key=selected
        val view=WebHost.pages[key]?.view
        if(view==null){done(reply);return}
        var completed=false
        val timeout=Runnable{if(!completed){completed=true;done(reply)}}
        handler.postDelayed(timeout,500)
        view.evaluateJavascript(command.getString("background_script")){value->
          if(!completed){
            completed=true;handler.removeCallbacks(timeout)
            val color=runCatching{JSONTokener(value).nextValue()}.getOrNull()
            if(color is String&&color.matches(Regex("#[0-9a-fA-F]{6}"))){
              val pages=reply.getJSONArray("pages")
              for(index in 0 until pages.length()){val page=pages.getJSONObject(index);if(page.optString("tab")==key?.removePrefix("${command.getString("profile")}-"))page.put("background",color)}
            }
            done(reply)
          }
        }
        return
      }
      if(action !in listOf("find","find_next","find_previous","find_close","screenshot")){done(control(owner,command));return}
      if(!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))throw IllegalStateException("Open the native client")
      val account=command.getString("profile")
      val tab=command.getString("tab")
      if(!tab.matches(Regex("[0-9a-f]{32}")))throw IllegalArgumentException("Invalid browser page")
      val key="$account-$tab"
      val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Browser page closed")

      if(action=="screenshot"){
        captureOwner(owner,view){bytes,error->
          if(error!=null)fail(error)
          else if(bytes!=null){
            val name="DL-page-${System.currentTimeMillis()}.png"
            if(Build.VERSION.SDK_INT<29){try{owner.saveBrowserScreenshot(bytes,name){saved,message->if(message!=null)fail(message)else done(JSONObject().put("destination",if(saved)"download" else "cancelled"))}}catch(error:Exception){fail(error.message ?: "Screenshot could not be saved")}}
            else Thread{
              var uri:android.net.Uri?=null
              try{
                val values=android.content.ContentValues().apply{put(android.provider.MediaStore.MediaColumns.DISPLAY_NAME,name);put(android.provider.MediaStore.MediaColumns.MIME_TYPE,"image/png");put(android.provider.MediaStore.MediaColumns.RELATIVE_PATH,android.os.Environment.DIRECTORY_DOWNLOADS+"/Daat Locus");put(android.provider.MediaStore.MediaColumns.IS_PENDING,1)}
                uri=owner.contentResolver.insert(android.provider.MediaStore.Downloads.EXTERNAL_CONTENT_URI,values) ?: throw java.io.IOException("Downloads unavailable")
                owner.contentResolver.openOutputStream(uri!!)?.use{it.write(bytes)} ?: throw java.io.IOException("Screenshot file unavailable")
                values.clear();values.put(android.provider.MediaStore.MediaColumns.IS_PENDING,0);owner.contentResolver.update(uri!!,values,null,null)
                done(JSONObject().put("destination","download"))
              }catch(_:Exception){uri?.let{runCatching{owner.contentResolver.delete(it,null,null)}};fail("Screenshot could not be saved")}
            }.start()
          }
        }
      }else{
        val text=command.optString("url","")
        if(action=="find_close"||text.isEmpty()){view.clearMatches();done(JSONObject().put("found",false).put("count",0).put("index",0));return}
        var completed=false
        val timeout=Runnable{if(!completed){completed=true;view.setFindListener(null);fail("Page search timed out")}}
        view.setFindListener{index,count,finished->if(finished&&!completed){completed=true;handler.removeCallbacks(timeout);view.setFindListener(null);done(JSONObject().put("found",count>0).put("count",count).put("index",if(count>0)index+1 else 0))}}
        handler.postDelayed(timeout,5000)
        if(action=="find")view.findAllAsync(text)else view.findNext(action!="find_previous")
      }
    }catch(error:Exception){fail(error.message ?: "Browser unavailable")}
  }
  private fun captureOwner(owner:MainActivity,view:WebView,done:(ByteArray?,String?)->Unit){
    val width=view.width;val height=view.height
    if(!embedded||!view.isShown||width<=0||height<=0||width.toLong()*height>64_000_000){done(null,"Open a browser page before capturing");return}
    cursor(view,JSONObject().put("kind","suspend")){
      view.postVisualStateCallback(SystemClock.uptimeMillis(),object:WebView.VisualStateCallback(){override fun onComplete(requestId:Long){
        if(!embedded||!view.isShown||!view.isAttachedToWindow){cursor(view,JSONObject().put("kind","resume"));done(null,"Browser page is no longer visible");return}
        val bitmap=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888)
        fun copied(success:Boolean){
          cursor(view,JSONObject().put("kind","resume"))
          if(!success){bitmap.recycle();done(null,"Screenshot unavailable");return}
          Thread{try{val bytes=ByteArrayOutputStream().also{bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}.toByteArray();owner.runOnUiThread{done(bytes,null)}}catch(_:Exception){owner.runOnUiThread{done(null,"Screenshot unavailable")}}finally{bitmap.recycle()}}.start()
        }
        try{
          // Snapshot this WebView only. Window PixelCopy would include shell
          // tooltips/dialogs composited above it, potentially exposing account UI.
          view.draw(Canvas(bitmap));copied(true)
        }catch(_:Exception){copied(false)}
      }})
    }
  }
  fun control(owner:MainActivity,command:JSONObject):JSONObject {
    profile=command.getString("profile")
    val action=command.getString("action")
    if(action=="detach"){
      composition?.let{it.page=null;it.inputRegions=emptyList()}
      content?.let{(it.parent as? ViewGroup)?.removeView(it)};embedded=false
      content?.let{workspace?.setContentView(it)};layoutOffscreen(owner)
      return JSONObject()
    }
    if(!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))throw IllegalStateException("Open the native client")
    val tab=command.optString("tab").takeIf{it.matches(Regex("[0-9a-f]{32}"))}
    val key=tab?.let{"$profile-$it"} ?: selected
    when(action){
      "suggestions","bookmarks","bookmark","save_bookmark","remove_bookmark","record_search","remove_search"->return saved(owner,action,JSONObject(command.optString("url","{}")))
      "mount"->{
        ensure(owner)
        val client=WebHost.clientView.get() ?: throw IllegalStateException("Client view unavailable")
        val bounds=command.getJSONObject("bounds")
        val scale=bounds.getDouble("scale").toFloat()
        val x=(bounds.getDouble("x")*scale).toInt().coerceIn(0,client.width)
        val y=(bounds.getDouble("y")*scale).toInt().coerceIn(0,client.height)
        val width=(bounds.getDouble("width")*scale).toInt().coerceIn(1,(client.width-x).coerceAtLeast(1))
        val height=(bounds.getDouble("height")*scale).toInt().coerceIn(1,(client.height-y).coerceAtLeast(1))
        val container=composition(owner,client)
        val frame=content!!
        if(frame.parent!==container){(frame.parent as? ViewGroup)?.removeView(frame);container.addView(frame,0,FrameLayout.LayoutParams(width,height))}
        container.page=frame
        val regions=command.optJSONArray("foreground") ?: JSONArray()
        val foreground=(0 until regions.length()).map{index->val region=regions.getJSONObject(index);val left=(region.getDouble("x")*scale).toFloat();val top=(region.getDouble("y")*scale).toFloat();android.graphics.RectF(left,top,left+(region.getDouble("width")*scale).toFloat(),top+(region.getDouble("height")*scale).toFloat())}
        container.update(x,y,width,height,foreground)
        embedded=true;frame.visibility=View.VISIBLE;
        if(selected==null)tabOrder.firstOrNull()?.let{select(it)}
      }
      "preferences"->return preferences(owner)
      "set_preferences"->return patchPreferences(owner,JSONObject(command.getString("url")))
      "list"->Unit
      "move_tab"->{
        val target=command.optString("url").takeIf{it!="null"&&it.isNotEmpty()}?.let{"$profile-$it"}
        if(key !in tabOrder||target!=null&&target !in tabOrder)throw IllegalArgumentException("Browser page closed")
        if(key!=target){tabOrder.remove(key);val index=target?.let{tabOrder.indexOf(it)} ?: tabOrder.size;tabOrder.add(index,key!!)}
      }
      "library"->return library(owner,command.optString("url").toIntOrNull() ?: 0)
      "clear_history"->{saveArray(owner,"history",JSONArray());saveArray(owner,"searches",JSONArray())}
      "remove_history"->{val id=command.getString("url");saveArray(owner,"history",filtered(array(owner,"history")){it.optString("id")!=id})}
      "remove_download"->{val id=command.getString("url");saveArray(owner,"downloads",filtered(array(owner,"downloads")){it.optString("id")!=id || it.optString("status")=="downloading"})}
      "save_download"->{
        val entries=array(owner,"downloads");val id=command.getString("url");val entry=(0 until entries.length()).map{entries.getJSONObject(it)}.find{it.getString("id")==id} ?: throw IllegalArgumentException("Download unavailable")
        val manager=owner.getSystemService(android.content.Context.DOWNLOAD_SERVICE) as android.app.DownloadManager
        val uri=manager.getUriForDownloadedFile(entry.getLong("native_id")) ?: throw IllegalStateException("Download unavailable")
        val intent=android.content.Intent(android.content.Intent.ACTION_SEND).setType(manager.getMimeTypeForDownloadedFile(entry.getLong("native_id")) ?: "application/octet-stream").putExtra(android.content.Intent.EXTRA_STREAM,uri).addFlags(android.content.Intent.FLAG_GRANT_READ_URI_PERMISSION)
        owner.startActivity(android.content.Intent.createChooser(intent,null))
      }
      "clear_data"->{
        val temporary=if(WebHost.pages.keys.none{it.startsWith("$profile-")}) "$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}" else null
        if(temporary!=null)WebHost.page(profile,temporary)
        WebHost.pages.filterKeys{it.startsWith("$profile-")}.values.forEach{page->androidx.webkit.WebViewCompat.getProfile(page.view).cookieManager.removeAllCookies(null);androidx.webkit.WebViewCompat.getProfile(page.view).webStorage.deleteAllData();page.view.clearCache(true)};if(temporary!=null)WebHost.close(temporary);
      }
      "zoom"->{val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page");val zoom=command.getString("url").toDouble();if(!zoom.isFinite()||zoom !in .25..5.0)throw IllegalArgumentException("Invalid zoom");view.zoomBy((zoom/(zooms[key] ?: 1.0)).toFloat());zooms[key!!]=zoom;}
      "print"->{val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page");val manager=owner.getSystemService(android.content.Context.PRINT_SERVICE) as android.print.PrintManager;manager.print(view.title ?: "Web page",view.createPrintDocumentAdapter(view.title ?: "Web page"),null)}
      "stop"->{WebHost.pages[key]?.view?.stopLoading()}

      "new_tab"->{val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";WebHost.page(profile,next).view.loadUrl("about:blank");select(next)}
      "open"->{val url=command.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";WebHost.page(profile,next).view.let{navigate(owner,next,it,url)};select(next)}
      "select"->{if(key==null||!WebHost.pages.containsKey(key))throw IllegalArgumentException("Browser page closed");select(key)}
      "close"->{if(key!=null)WebHost.close(key)}
      "navigate","back","forward","reload"->{
        val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page")

        configure(owner,view)
        when(action){"navigate"->{val url=command.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");navigate(owner,key!!,view,url)};"back"->view.goBack();"forward"->view.goForward();else->view.reload()}
      }
      else->throw IllegalArgumentException("Unsupported browser control")
    }
    return JSONObject().put("pages",pages(profile)).put("selected",selected?.removePrefix("$profile-") ?: JSONObject.NULL)
  }
  private val zooms=mutableMapOf<String,Double>()
  private fun saved(owner:MainActivity,action:String,input:JSONObject):JSONObject{
    val query=input.optString("query","").trim()
    if(query.length>4096)throw IllegalArgumentException("Search is too long")
    val words=query.lowercase(java.util.Locale.ROOT).split(Regex("\\s+")).filter{it.isNotEmpty()}
    fun entries(key:String):List<JSONObject>{val values=array(owner,key);return (0 until values.length()).map{values.getJSONObject(it)}}
    fun matches(value:JSONObject):Boolean{val text=(value.optString("title")+" "+value.optString("url")+" "+value.optString("query")).lowercase(java.util.Locale.ROOT);return words.all{text.contains(it)}}
    val bookmarks=entries("bookmarks")
    val id=input.optString("id").takeUnless{it.isEmpty()||it=="null"}
    when(action){
      "suggestions"->return JSONObject().put("history",JSONArray(entries("history").asReversed().filter(::matches).distinctBy{it.optString("url")}.take(8))).put("bookmarks",JSONArray(bookmarks.asReversed().filter(::matches).take(8))).put("searches",JSONArray(entries("searches").asReversed().filter(::matches).take(8)))
      "bookmarks"->{val offset=input.optInt("offset",0).coerceAtLeast(0);val result=bookmarks.asReversed().filter(::matches).drop(offset).take(101);return JSONObject().put("items",JSONArray(result.take(100))).put("next_offset",if(result.size>100)offset+100 else JSONObject.NULL)}
      "bookmark"->return JSONObject().put("item",bookmarks.find{it.optString("id")==id||it.optString("url")==input.optString("url")} ?: JSONObject.NULL)
      "save_bookmark"->{
        val raw=input.getString("url");val uri=java.net.URI(raw)
        if(uri.scheme !in listOf("http","https")||uri.host.isNullOrEmpty()||uri.rawUserInfo!=null)throw IllegalArgumentException("Invalid bookmark URL")
        val url=if(uri.rawPath.isNullOrEmpty()&&uri.rawQuery==null&&uri.rawFragment==null)"$raw/" else raw
        val title=input.optString("title").trim().ifEmpty{uri.host}
        if(title.length>4096)throw IllegalArgumentException("Bookmark name is too long")
        if(id!=null&&bookmarks.none{it.optString("id")==id})throw IllegalArgumentException("Bookmark no longer exists")
        if(id!=null&&bookmarks.any{it.optString("url")==url&&it.optString("id")!=id})throw IllegalArgumentException("This page is already bookmarked")
        val old=bookmarks.find{if(id!=null)it.optString("id")==id else it.optString("url")==url}
        val icon=input.optString("favicon").takeIf{it.startsWith("data:image/png;base64,")&&it.length<48000} ?: old?.takeIf{runCatching{val previous=java.net.URI(it.getString("url"));previous.scheme==uri.scheme&&previous.rawAuthority.equals(uri.rawAuthority,true)}.getOrDefault(false)}?.optString("favicon")
        val entry=JSONObject().put("id",old?.getString("id") ?: java.util.UUID.randomUUID().toString().replace("-","")).put("title",title).put("url",url).put("time",System.currentTimeMillis()).put("favicon",icon ?: JSONObject.NULL)
        saveArray(owner,"bookmarks",JSONArray(bookmarks.filter{it.optString("id")!=entry.getString("id")}+entry));changed();return entry
      }
      "remove_bookmark"->saveArray(owner,"bookmarks",JSONArray(bookmarks.filter{it.optString("id")!=id}))
      "record_search"->{if(query.isEmpty())throw IllegalArgumentException("Enter a search");val entries=entries("searches").filter{!it.optString("query").equals(query,true)};saveArray(owner,"searches",JSONArray(entries+JSONObject().put("id",java.util.UUID.randomUUID().toString().replace("-","")).put("query",query).put("time",System.currentTimeMillis())))}
      "remove_search"->saveArray(owner,"searches",JSONArray(entries("searches").filter{it.optString("id")!=id}))
    }
    changed();return JSONObject()
  }
  private fun array(owner:MainActivity,key:String)=try{JSONArray(owner.getSharedPreferences("browser-library",0).getString(key,"[]"))}catch(_:Exception){JSONArray()}
  private fun saveArray(owner:MainActivity,key:String,value:JSONArray){if(!owner.getSharedPreferences("browser-library",0).edit().putString(key,value.toString()).commit())throw IllegalStateException("Browser library could not be saved")}
  private fun filtered(source:JSONArray,keep:(JSONObject)->Boolean)=JSONArray().also{result->for(index in 0 until source.length()){val value=source.getJSONObject(index);if(keep(value))result.put(value)}}
  fun visit(owner:MainActivity,url:String,title:String){
    if(!WebHost.allowed(url))return
    val history=array(owner,"history")
    if(history.length()>0&&history.getJSONObject(history.length()-1).optString("url")==url){history.getJSONObject(history.length()-1).put("title",title).put("time",System.currentTimeMillis())}
    else history.put(JSONObject().put("favicon",WebHost.pages.entries.firstOrNull{it.value.view.url==url}?.key?.let{icons[it]} ?: JSONObject.NULL).put("id",java.util.UUID.randomUUID().toString()).put("url",url).put("title",title).put("time",System.currentTimeMillis()))
    saveArray(owner,"history",history)
  }
  fun download(owner:MainActivity,view:WebView,url:String,agent:String?,disposition:String?,mime:String?){
    if(!WebHost.allowed(url))return
    try{
      val id=java.util.UUID.randomUUID().toString();val name=URLUtil.guessFileName(url,disposition,mime).replace(Regex("[\\\\/:*?\\\"<>|]"),"_").take(180)
      val request=android.app.DownloadManager.Request(android.net.Uri.parse(url)).setTitle(name).setNotificationVisibility(android.app.DownloadManager.Request.VISIBILITY_VISIBLE_NOTIFY_COMPLETED).setDestinationInExternalFilesDir(owner,android.os.Environment.DIRECTORY_DOWNLOADS,"browser/$id/$name")
      if(agent!=null)request.addRequestHeader("User-Agent",agent)
      androidx.webkit.WebViewCompat.getProfile(view).cookieManager.getCookie(url)?.let{request.addRequestHeader("Cookie",it)}
      val manager=owner.getSystemService(android.content.Context.DOWNLOAD_SERVICE) as android.app.DownloadManager
      val native=manager.enqueue(request);val downloads=array(owner,"downloads")
      downloads.put(JSONObject().put("id",id).put("native_id",native).put("url",url).put("name",name).put("time",System.currentTimeMillis()).put("status","downloading").put("bytes",0));saveArray(owner,"downloads",downloads)
    }catch(_:Exception){val downloads=array(owner,"downloads");downloads.put(JSONObject().put("id",java.util.UUID.randomUUID().toString()).put("url",url).put("name",URLUtil.guessFileName(url,disposition,mime)).put("time",System.currentTimeMillis()).put("status","failed").put("bytes",0));saveArray(owner,"downloads",downloads)}
  }
  private fun library(owner:MainActivity,offset:Int):JSONObject {
    val downloads=array(owner,"downloads");val history=array(owner,"history");val manager=owner.getSystemService(android.content.Context.DOWNLOAD_SERVICE) as android.app.DownloadManager
    for(index in 0 until downloads.length()){
      val item=downloads.getJSONObject(index);if(!item.has("native_id")||item.optString("status")!="downloading")continue
      manager.query(android.app.DownloadManager.Query().setFilterById(item.getLong("native_id"))).use{cursor->
        if(cursor!=null&&cursor.moveToFirst()){val status=cursor.getInt(cursor.getColumnIndexOrThrow(android.app.DownloadManager.COLUMN_STATUS));item.put("status",when(status){android.app.DownloadManager.STATUS_SUCCESSFUL->"complete";android.app.DownloadManager.STATUS_FAILED->"failed";else->"downloading"});item.put("bytes",cursor.getLong(cursor.getColumnIndexOrThrow(android.app.DownloadManager.COLUMN_BYTES_DOWNLOADED_SO_FAR)))}else item.put("status","interrupted")
      }
    }
    saveArray(owner,"downloads",downloads)
    fun window(source:JSONArray)=JSONArray().also{result->for(index in (source.length()-1-offset) downTo (source.length()-offset-100).coerceAtLeast(0)){if(index>=0){val item=JSONObject(source.getJSONObject(index).toString());item.remove("native_id");result.put(item)}}}
    val next=offset+100
    return JSONObject().put("history",window(history)).put("downloads",window(downloads)).put("next_offset",if(maxOf(history.length(),downloads.length())>next)next else JSONObject.NULL)
  }
  private data class ImageFrame(val id:String,val width:Int,val height:Int)
  private val frames=mutableMapOf<String,java.util.ArrayDeque<ImageFrame>>()
  private val jobs=mutableMapOf<String,String>()
  private val loaded=mutableMapOf<String,()->Unit>()
  private val handler=Handler(Looper.getMainLooper())
  private fun pageInfo(key:String,profile:String,page:WebHost.Page)=JSONObject().put("tab",key.removePrefix("$profile-")).put("url",requestedUrls[key] ?: page.view.url ?: "about:blank").put("title",if(page.view.url=="about:blank")"" else page.view.title ?: "").put("can_back",page.view.canGoBack()).put("can_forward",page.view.canGoForward()).put("zoom",zooms[key] ?: 1.0).put("loading",requestedUrls.containsKey(key)||page.view.progress<100).put("progress",page.view.progress).put("favicon",icons[key] ?: JSONObject.NULL)
  private fun pages(profile:String)=JSONArray().also { result -> WebHost.pages.filter { (key,page)->key.startsWith("$profile-")&&(requestedUrls.containsKey(key)||page.view.url==null||page.view.url=="about:blank"||WebHost.allowed(page.view.url ?: "")) }.entries.sortedBy{tabOrder.indexOf(it.key)}.forEach { (key,page)->result.put(pageInfo(key,profile,page)) } }
  private fun shortcut(code:Int,event:KeyEvent):String?{
    val command=event.isCtrlPressed||event.isMetaPressed;val shift=event.isShiftPressed
    if(command&&!event.isAltPressed){
      if(code==KeyEvent.KEYCODE_TAB)return if(shift)"previous-tab" else "next-tab"
      if(code==KeyEvent.KEYCODE_G)return if(shift)"find-previous" else "find-next"
      if(code==KeyEvent.KEYCODE_EQUALS||code==KeyEvent.KEYCODE_PLUS||code==KeyEvent.KEYCODE_NUMPAD_ADD)return "zoom-in"
      if(code==KeyEvent.KEYCODE_O&&shift)return "bookmarks"
      if(shift)return null
      return when(code){
        KeyEvent.KEYCODE_PAGE_UP->"previous-tab";KeyEvent.KEYCODE_PAGE_DOWN->"next-tab"
        KeyEvent.KEYCODE_MINUS,KeyEvent.KEYCODE_NUMPAD_SUBTRACT->"zoom-out";KeyEvent.KEYCODE_0->"zoom-reset"
        in KeyEvent.KEYCODE_1..KeyEvent.KEYCODE_9->"tab-${code-KeyEvent.KEYCODE_0}"
        KeyEvent.KEYCODE_D->"d";KeyEvent.KEYCODE_L->"l";KeyEvent.KEYCODE_T->"t";KeyEvent.KEYCODE_W->"w";KeyEvent.KEYCODE_H->"h";KeyEvent.KEYCODE_J->"j";KeyEvent.KEYCODE_F->"f";KeyEvent.KEYCODE_P->"p";KeyEvent.KEYCODE_R->"r";else->null
      }
    }
    if(event.isAltPressed&&!command&&!shift)return when(code){KeyEvent.KEYCODE_DPAD_LEFT->"back";KeyEvent.KEYCODE_DPAD_RIGHT->"forward";KeyEvent.KEYCODE_D->"l";else->null}
    if(!command&&!event.isAltPressed)return when(code){KeyEvent.KEYCODE_F5->if(shift)null else "r";KeyEvent.KEYCODE_F3->if(shift)"find-previous" else "find-next";else->null}
    return null
  }
  private fun response(id:String,browser:JSONObject?=null,error:String?=null)=JSONObject().put("id",id).put("capture",JSONObject.NULL).put("browser",browser ?: JSONObject.NULL).put("error",error ?: JSONObject.NULL).toString()
  private fun metrics(view:WebView,script:String,done:(JSONObject)->Unit,fail:(String)->Unit) {
    view.evaluateJavascript(script) { value -> try { val decoded=JSONTokener(value).nextValue();done(if(decoded is String)JSONObject(decoded)else decoded as JSONObject) }catch(_:Exception){fail("Browser inspection failed")} }
  }
  // Synchronize with a published renderer frame, never with Owner idleness or
  // a stable DOM. Pages and Owner input remain live while the callback is pending.
  private fun frameReady(view:WebView,done:()->Unit,fail:(String)->Unit){
    var completed=false
    val timeout=Runnable{if(!completed){completed=true;fail("Browser rendering is unavailable; observe before continuing")}}
    handler.postDelayed(timeout,8000)
    view.postVisualStateCallback(SystemClock.uptimeMillis(),object:WebView.VisualStateCallback(){
      override fun onComplete(requestId:Long){
        if(completed)return
        completed=true;handler.removeCallbacks(timeout);done()
      }
    })
    view.invalidate()
  }
  fun request(owner:MainActivity,profile:String,id:String,command:JSONObject,script:String,cursor:String):String {
    cursorScript=cursor
    val result=CompletableFuture<String>()
    val tab=command.optString("tab")
    val key="$profile-$tab"
    fun fail(message:String) { if(jobs[key]==id){jobs.remove(key);loaded.remove(key);};result.complete(response(id,error=message)) }
    fun finish(observation:JSONObject?=null){if(jobs[key]==id){jobs.remove(key);loaded.remove(key)};result.complete(response(id,JSONObject().put("pages",pages(profile)).put("observation",observation ?: JSONObject.NULL)))}
    fun captureFrame() {
      if(result.isDone||jobs[key]!=id)return
      val page=WebHost.pages[key] ?: run {fail("Browser page closed");return}
      val view=page.view
      val width=view.width;val height=view.height
      if(width<=0||height<=0||width.toLong()*height>64_000_000){fail("Browser viewport exceeds image budget");return}
      val bitmap=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888)
      fun copied(success:Boolean) {
        cursor(view,JSONObject().put("kind","resume"))
        try {
          if(result.isDone||jobs[key]!=id)return
          if(!WebHost.pages.containsKey(key)){fail("Browser page closed");return}
          if(!success){fail("Browser screenshot unavailable");return}
          val bytes=ByteArrayOutputStream().also {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}.toByteArray()
          if(bytes.size>7*1024*1024)throw IllegalStateException("Browser screenshot exceeds its budget")
          val images=frames.getOrPut(key){java.util.ArrayDeque()};images.addLast(ImageFrame(id,width,height));while(images.size>128)images.removeFirst()
          finish(JSONObject().put("page",pageInfo(key,profile,page)).put("state_id",id).put("width",width).put("height",height).put("png_base64",Base64.encodeToString(bytes,Base64.NO_WRAP)))
        }catch(error:Exception){fail(error.message ?: "Browser capture failed")}finally{bitmap.recycle()}
      }
      // This is an explicit page snapshot, never a frame used for composition.
      // Capture the page itself even when a noninteractive shell overlay is up.
      view.draw(Canvas(bitmap));copied(true)
    }
    fun capture(){
      val view=WebHost.pages[key]?.view ?: run{fail("Browser page closed");return}
      cursor(view,JSONObject().put("kind","prepare")){cursor(view,JSONObject().put("kind","suspend")){frameReady(view,{captureFrame()},::fail)}}
    }
    fun client(page:WebHost.Page) {
      page.view.webViewClient=object:WebViewClient(){
        override fun shouldOverrideUrlLoading(view:WebView,request:WebResourceRequest)=!WebHost.allowed(request.url.toString())
        override fun onPageStarted(view:WebView,url:String,favicon:Bitmap?){changed()}
        override fun onPageFinished(view:WebView,url:String){finished(owner,key,view,url);if(!WebHost.allowed(url)){fail("Browser URL is not allowed");return};loaded.remove(key)?.invoke()}
        override fun onReceivedSslError(view:WebView,handler:SslErrorHandler,error:SslError){handler.cancel();if(jobs[key]==id)fail("Browser certificate validation failed")}
        override fun onReceivedError(view:WebView,request:WebResourceRequest,error:WebResourceError){if(request.isForMainFrame){if(jobs[key]==id)fail("Browser navigation failed")}}
      }
    }
    owner.runOnUiThread {
      if(result.isDone)return@runOnUiThread
      try {
        if(owner.isFinishing||owner.isDestroyed)throw IllegalStateException("Native browser Activity is unavailable")
        val action=command.getString("action")
        if(action=="list"){finish();return@runOnUiThread}
        if(!tab.matches(Regex("[0-9a-f]{32}")))throw IllegalArgumentException("Invalid browser page")
        jobs[key]=id
        when(action){
          "close"->{WebHost.close(key);finish()}
          "new_tab"->{val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";WebHost.page(profile,next).view.loadUrl("about:blank")}
      "open"->{
            val url=command.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed")
            if(WebHost.pages.containsKey(key))throw IllegalStateException("Browser page already exists; observe instead of replaying open")
            val page=WebHost.page(profile,key);client(page);loaded[key]={handler.postDelayed({capture()},180)};navigate(owner,key,page.view,url)
          }
          "observe"->{if(!WebHost.pages.containsKey(key))throw IllegalStateException("Browser page closed");capture()}
          "input"->{
            val page=WebHost.pages[key] ?: throw IllegalStateException("Browser page closed")
            val images=frames[key]
            val coordinateSize=command.optJSONArray("coordinate_size")
            val frame=if(coordinateSize!=null) ImageFrame(command.getString("state_id"),coordinateSize.getInt(0),coordinateSize.getInt(1)) else images?.firstOrNull{it.id==command.getString("state_id")} ?: images?.lastOrNull() ?: ImageFrame("",page.view.width.coerceAtLeast(1),page.view.height.coerceAtLeast(1))
            frameReady(page.view,{ metrics(page.view,script,{value ->
              try {
                if(result.isDone||jobs[key]!=id)return@metrics
                val input=command.getJSONObject("input");val kind=input.getString("kind");if(kind in listOf("navigate","back","forward","reload"))configure(owner,page.view)
                fun point():Pair<Float,Float>{val x=input.getDouble("x");val y=input.getDouble("y");if(!x.isFinite()||!y.isFinite()||x<0||y<0||x>=frame.width||y>=frame.height)throw IllegalArgumentException("Point outside observed browser image");return Pair((x/frame.width*page.view.width).toFloat(),(y/frame.height*page.view.height).toFloat())}
                client(page)
                val hint=JSONObject().put("kind",kind)
                when(kind){
                  "move","click","scroll","drag"->{point();hint.put("x",input.getDouble("x")/frame.width*value.getDouble("width")).put("y",input.getDouble("y")/frame.height*value.getDouble("height"))}
                }
                if(kind=="click")hint.put("count",input.getInt("count"))
                if(kind=="scroll")hint.put("delta_x",input.getDouble("delta_x")).put("delta_y",input.getDouble("delta_y"))
                if(kind in listOf("navigate","back","forward","reload"))hint.put("kind","hide")
                if(kind=="drag"){hint.put("to_x",input.getDouble("to_x")/frame.width*value.getDouble("width")).put("to_y",input.getDouble("to_y")/frame.height*value.getDouble("height"))}
                cursor(page.view,hint){
                try{
                  if(result.isDone||jobs[key]!=id)return@cursor
                  if(!WebHost.pages.containsKey(key)){fail("Browser page closed");return@cursor}
                  modelInput {
                  if(page.view.parent===parking[key]?.root){
                    page.view.requestFocus()
                    page.view.onWindowFocusChanged(true)
                  }
                  when(kind){
                  "move"->{val(x,y)=point();mouse(page.view,MotionEvent.ACTION_HOVER_MOVE,x,y,0)}
                  "drag"->{val(x,y)=point();val toX=input.getDouble("to_x");val toY=input.getDouble("to_y");if(!toX.isFinite()||!toY.isFinite()||toX<0||toY<0||toX>=frame.width||toY>=frame.height)throw IllegalArgumentException("Drag endpoint outside image");val endX=(toX/frame.width*page.view.width).toFloat();val endY=(toY/frame.height*page.view.height).toFloat();mouse(page.view,MotionEvent.ACTION_DOWN,x,y,MotionEvent.BUTTON_PRIMARY);for(step in 1..12){val t=step/12f;mouse(page.view,MotionEvent.ACTION_MOVE,x+(endX-x)*t,y+(endY-y)*t,MotionEvent.BUTTON_PRIMARY)};mouse(page.view,MotionEvent.ACTION_UP,endX,endY,0,actionButton=MotionEvent.BUTTON_PRIMARY)}
                  "click"->{
                    val(x,y)=point()
                    val button=when(input.getString("button")){"right"->MotionEvent.BUTTON_SECONDARY;"middle"->MotionEvent.BUTTON_TERTIARY;else->MotionEvent.BUTTON_PRIMARY}
                    repeat(input.getInt("count")){
                      mouse(page.view,MotionEvent.ACTION_DOWN,x,y,button)
                      mouse(page.view,MotionEvent.ACTION_UP,x,y,0,actionButton=button)
                    }
                  }
                  "scroll"->{val(x,y)=point();mouse(page.view,MotionEvent.ACTION_HOVER_MOVE,x,y,0);mouse(page.view,MotionEvent.ACTION_SCROLL,x,y,0,(-input.getDouble("delta_x")/48).toFloat(),(-input.getDouble("delta_y")/48).toFloat())}
                  "type"->{val connection=page.view.onCreateInputConnection(EditorInfo());if(connection==null||!connection.commitText(input.getString("text"),1))throw IllegalStateException("Focus an editable field before typing")}
                  "key"->{val code=keyCode(input.getString("key"));val modifiers=input.getJSONArray("modifiers");var meta=0;for(i in 0 until modifiers.length())meta=meta or when(modifiers.getString(i)){"control"->KeyEvent.META_CTRL_ON;"alt"->KeyEvent.META_ALT_ON;"shift"->KeyEvent.META_SHIFT_ON;"meta"->KeyEvent.META_META_ON;else->throw IllegalArgumentException("Unsupported modifier")};val now=SystemClock.uptimeMillis();page.view.dispatchKeyEvent(KeyEvent(now,now,KeyEvent.ACTION_DOWN,code,0,meta));page.view.dispatchKeyEvent(KeyEvent(now,now,KeyEvent.ACTION_UP,code,0,meta))}
                  "navigate"->{val url=input.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");loaded[key]={handler.postDelayed({capture()},180)};navigate(owner,key,page.view,url)}
                  "back"->{if(!page.view.canGoBack())throw IllegalStateException("No previous browser page");loaded[key]={handler.postDelayed({capture()},180)};page.view.goBack()}
                  "forward"->{if(!page.view.canGoForward())throw IllegalStateException("No next browser page");loaded[key]={handler.postDelayed({capture()},180)};page.view.goForward()}
                  "reload"->{loaded[key]={handler.postDelayed({capture()},180)};page.view.reload()}
                  else->throw IllegalArgumentException("Unsupported browser input")
                } }
                if(!loaded.containsKey(key))handler.postDelayed({capture()},250)
                }catch(error:Exception){fail(error.message ?: "Browser input outcome unknown; observe again")}
                }
              }catch(error:Exception){fail(error.message ?: "Browser input outcome unknown; observe again")}
            },::fail) },::fail)
          }
          else->throw IllegalArgumentException("Unknown browser operation")
        }
      }catch(error:Exception){fail(error.message ?: "Browser operation failed")}
    }
    return try{result.get(27,TimeUnit.SECONDS)}catch(_:Exception){result.complete(response(id,error="Browser outcome unknown; observe before continuing"));owner.runOnUiThread{if(jobs[key]==id){jobs.remove(key);loaded.remove(key);}};response(id,error="Browser outcome unknown; do not replay input")}
  }
  private fun cursor(view:WebView,event:JSONObject,done:(()->Unit)?=null){
    if(cursorScript.isEmpty()){done?.invoke();return}
    try{view.evaluateJavascript("($cursorScript)($event)"){done?.invoke()}}catch(_:Exception){done?.invoke()}
  }
  // Android's public MotionEvent constructors omit the changed mouse button.
  // This availability-checked platform setter respects hidden-API enforcement;
  // no global event injection, permission change or enforcement bypass is used.
  private val mouseActionButton by lazy {
    try{MotionEvent::class.java.getMethod("setActionButton",Int::class.javaPrimitiveType)}
    catch(error:ReflectiveOperationException){throw IllegalStateException("Native browser mouse buttons are unavailable",error)}
  }
  private fun mouse(view:WebView,action:Int,x:Float,y:Float,buttons:Int,horizontal:Float=0f,vertical:Float=0f,actionButton:Int=0){
    val properties=MotionEvent.PointerProperties().apply{id=0;toolType=MotionEvent.TOOL_TYPE_MOUSE}
    val coords=MotionEvent.PointerCoords().apply{this.x=x;this.y=y;setAxisValue(MotionEvent.AXIS_HSCROLL,horizontal);setAxisValue(MotionEvent.AXIS_VSCROLL,vertical)}
    // Chromium intentionally consumes mouse ACTION_DOWN/UP without sending DOM
    // events. Its native mouse path requires BUTTON_PRESS/RELEASE instead.
    val nativeAction=when(action){MotionEvent.ACTION_DOWN->MotionEvent.ACTION_BUTTON_PRESS;MotionEvent.ACTION_UP->MotionEvent.ACTION_BUTTON_RELEASE;else->action}
    val changed=when(action){MotionEvent.ACTION_DOWN->buttons;MotionEvent.ACTION_UP->actionButton;else->0}
    val setter=if(changed!=0)mouseActionButton else null
    val now=SystemClock.uptimeMillis()
    val event=MotionEvent.obtain(now,now,nativeAction,1,arrayOf(properties),arrayOf(coords),0,buttons,1f,1f,0,0,InputDevice.SOURCE_MOUSE,0)
    try{
      setter?.invoke(event,changed)
      if(action==MotionEvent.ACTION_SCROLL||action==MotionEvent.ACTION_HOVER_MOVE)view.dispatchGenericMotionEvent(event)
      else view.onTouchEvent(event)
    }finally{event.recycle()}
  }
  private fun keyCode(key:String):Int=when(key){
    "Enter"->KeyEvent.KEYCODE_ENTER;"Tab"->KeyEvent.KEYCODE_TAB;"Escape"->KeyEvent.KEYCODE_ESCAPE;"Backspace"->KeyEvent.KEYCODE_DEL;"Delete"->KeyEvent.KEYCODE_FORWARD_DEL
    "ArrowUp"->KeyEvent.KEYCODE_DPAD_UP;"ArrowDown"->KeyEvent.KEYCODE_DPAD_DOWN;"ArrowLeft"->KeyEvent.KEYCODE_DPAD_LEFT;"ArrowRight"->KeyEvent.KEYCODE_DPAD_RIGHT
    "Home"->KeyEvent.KEYCODE_MOVE_HOME;"End"->KeyEvent.KEYCODE_MOVE_END;"PageUp"->KeyEvent.KEYCODE_PAGE_UP;"PageDown"->KeyEvent.KEYCODE_PAGE_DOWN;"Space"->KeyEvent.KEYCODE_SPACE
    "F3"->KeyEvent.KEYCODE_F3;"F5"->KeyEvent.KEYCODE_F5;"+"->KeyEvent.KEYCODE_PLUS;"="->KeyEvent.KEYCODE_EQUALS;"-"->KeyEvent.KEYCODE_MINUS
    else->if(key.length==1&&key[0].isLetter()&&key[0].code<128)KeyEvent.KEYCODE_A+(key[0].lowercaseChar()-'a')else if(key.length==1&&key[0].isDigit())KeyEvent.KEYCODE_0+(key[0]-'0')else throw IllegalArgumentException("Unsupported browser key")
  }
}
