package io.shadow3aaa.daatlocus

import android.app.Dialog
import android.view.View
import android.view.ViewGroup
import android.widget.*
import android.view.inputmethod.EditorInfo
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Rect
import android.net.http.SslError
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Base64
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.view.PixelCopy
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
  private data class Cut(val rect:android.graphics.RectF,val radius:Float){
    fun contains(x:Float,y:Float):Boolean{if(!rect.contains(x,y))return false;val r=radius.coerceAtMost(minOf(rect.width(),rect.height())/2);val cx=x.coerceIn(rect.left+r,rect.right-r);val cy=y.coerceIn(rect.top+r,rect.bottom-r);return (x-cx)*(x-cx)+(y-cy)*(y-cy)<=r*r}
  }
  private class PageFrame(owner:MainActivity):FrameLayout(owner){
    var cuts:List<Cut> = emptyList()
    private var pageGesture=false
    override fun dispatchDraw(canvas:Canvas){
      val saved=canvas.save()
      for(cut in cuts){val path=android.graphics.Path();path.addRoundRect(cut.rect,cut.radius,cut.radius,android.graphics.Path.Direction.CW);if(Build.VERSION.SDK_INT>=26)canvas.clipOutPath(path)else{@Suppress("DEPRECATION") canvas.clipPath(path,android.graphics.Region.Op.DIFFERENCE)}}
      super.dispatchDraw(canvas);canvas.restoreToCount(saved)
    }
    override fun dispatchTouchEvent(event:MotionEvent):Boolean{
      if(event.actionMasked==MotionEvent.ACTION_DOWN)pageGesture=cuts.none{it.contains(event.x,event.y)}
      if(!pageGesture)return false
      return super.dispatchTouchEvent(event)
    }
    override fun dispatchGenericMotionEvent(event:MotionEvent):Boolean=if(cuts.any{it.contains(event.x,event.y)})false else super.dispatchGenericMotionEvent(event)
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
  private val tabOrder=mutableListOf<String>()
  private var profile=""
  private var embedded=false
  private var cursorScript=""
  fun shutdown(){workspace?.dismiss();content?.let{(it.parent as? ViewGroup)?.removeView(it)};workspace=null;content=null;selected=null;embedded=false;receipts.clear();jobs.clear();loaded.clear();tabOrder.clear()}
  private fun ensure(owner:MainActivity):Dialog {
    workspace?.let{return it}
    val dialog=Dialog(owner)
    content=PageFrame(owner)
    dialog.setContentView(content!!)
    workspace=dialog
    return dialog
  }
  fun mount(owner:MainActivity,key:String,view:WebView):Dialog {
    profile=key.substringBefore('-')
    val dialog=ensure(owner)
    configure(owner,view)
    content?.addView(view,FrameLayout.LayoutParams(-1,-1))
    view.setOnTouchListener{_,event->if(event.action==MotionEvent.ACTION_DOWN)receipts.remove(key);false}
    view.setOnKeyListener{_,code,event->
      if(event.action!=KeyEvent.ACTION_DOWN)return@setOnKeyListener false
      val letter=shortcut(code,event) ?: return@setOnKeyListener false
      receipts.remove(key);WebHost.clientView.get()?.let{client->client.requestFocus();client.evaluateJavascript("window.dispatchEvent(new CustomEvent('dl-browser-shortcut',{detail:'$letter'}))",null)};true
    }
    selected=key
    if(key !in tabOrder)tabOrder.add(key)
    view.visibility=View.VISIBLE
    layoutOffscreen(owner)
    handler.post{select(key)}
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
  fun remove(key:String){requestedUrls.remove(key);icons.remove(key);zooms.remove(key);val index=tabOrder.indexOf(key);tabOrder.remove(key);if(selected==key)selected=tabOrder.filter{it.startsWith("$profile-")}.let{it.getOrNull(index.coerceAtMost(it.lastIndex))};selected?.let{select(it)}}
  private fun select(key:String){if(!WebHost.pages.containsKey(key))return;selected=key;WebHost.pages.filterKeys{it.startsWith("$profile-")}.forEach{(id,page)->page.view.visibility=if(id==key)View.VISIBLE else View.GONE}}
  fun controlAsync(owner:MainActivity,command:JSONObject,done:(JSONObject)->Unit){
    fun fail(message:String){done(JSONObject().put("error",message))}
    try{
      val action=command.getString("action")
      if(action !in listOf("find","find_next","find_previous","find_close","screenshot")){done(control(owner,command));return}
      if(!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))throw IllegalStateException("Open the native client")
      val account=command.getString("profile")
      val tab=command.getString("tab")
      if(!tab.matches(Regex("[0-9a-f]{32}")))throw IllegalArgumentException("Invalid browser page")
      val key="$account-$tab"
      val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Browser page closed")
      receipts.remove(key)
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
    if(!embedded||!view.isShown||width<=0||height<=0||width.toLong()*height>8_000_000){done(null,"Open a browser page before capturing");return}
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
          if(Build.VERSION.SDK_INT>=26){val location=IntArray(2);view.getLocationInWindow(location);PixelCopy.request(owner.window,Rect(location[0],location[1],location[0]+width,location[1]+height),bitmap,{status->copied(status==PixelCopy.SUCCESS)},handler)}
          else{view.draw(Canvas(bitmap));copied(true)}
        }catch(_:Exception){copied(false)}
      }})
    }
  }
  fun control(owner:MainActivity,command:JSONObject):JSONObject {
    profile=command.getString("profile")
    val action=command.getString("action")
    if(action=="detach"){
      (content as? PageFrame)?.cuts=emptyList()
      content?.let{(it.parent as? ViewGroup)?.removeView(it)};embedded=false
      content?.let{workspace?.setContentView(it)};receipts.clear();layoutOffscreen(owner)
      return JSONObject()
    }
    if(!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))throw IllegalStateException("Open the native client")
    val tab=command.optString("tab").takeIf{it.matches(Regex("[0-9a-f]{32}"))}
    val key=tab?.let{"$profile-$it"} ?: selected
    when(action){
      "mount"->{
        ensure(owner)
        val client=WebHost.clientView.get() ?: throw IllegalStateException("Client view unavailable")
        val bounds=command.getJSONObject("bounds")
        val scale=bounds.getDouble("scale").toFloat()
        val x=(bounds.getDouble("x")*scale).toInt().coerceIn(0,client.width)
        val y=(bounds.getDouble("y")*scale).toInt().coerceIn(0,client.height)
        val width=(bounds.getDouble("width")*scale).toInt().coerceIn(1,(client.width-x).coerceAtLeast(1))
        val height=(bounds.getDouble("height")*scale).toInt().coerceIn(1,(client.height-y).coerceAtLeast(1))
        val origin=IntArray(2);client.getLocationInWindow(origin)
        val container=owner.window.decorView as ViewGroup
        val location=IntArray(2);container.getLocationInWindow(location)
        val frame=content!!
        if(frame.parent!==container){(frame.parent as? ViewGroup)?.removeView(frame);container.addView(frame)}
        frame.layoutParams=ViewGroup.LayoutParams(width,height)
        frame.x=(origin[0]-location[0]+x).toFloat();frame.y=(origin[1]-location[1]+y).toFloat()
        val cuts=command.optJSONArray("occlusions") ?: JSONArray()
        (frame as PageFrame).cuts=(0 until cuts.length()).map{index->val cut=cuts.getJSONObject(index);val left=((cut.getDouble("x")-bounds.getDouble("x"))*scale).toFloat();val top=((cut.getDouble("y")-bounds.getDouble("y"))*scale).toFloat();val w=(cut.getDouble("width")*scale).toFloat();val h=(cut.getDouble("height")*scale).toFloat();Cut(android.graphics.RectF(left,top,left+w,top+h),(cut.getDouble("radius")*scale).toFloat().coerceAtMost(minOf(w,h)/2))}
        frame.invalidate()
        embedded=true;frame.visibility=View.VISIBLE;receipts.clear()
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
      "clear_history"->{saveArray(owner,"history",JSONArray())}
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
        WebHost.pages.filterKeys{it.startsWith("$profile-")}.values.forEach{page->androidx.webkit.WebViewCompat.getProfile(page.view).cookieManager.removeAllCookies(null);androidx.webkit.WebViewCompat.getProfile(page.view).webStorage.deleteAllData();page.view.clearCache(true)};if(temporary!=null)WebHost.close(temporary);receipts.clear()
      }
      "zoom"->{val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page");val zoom=command.getString("url").toDouble();if(!zoom.isFinite()||zoom !in .25..5.0)throw IllegalArgumentException("Invalid zoom");view.zoomBy((zoom/(zooms[key] ?: 1.0)).toFloat());zooms[key!!]=zoom;receipts.clear()}
      "print"->{val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page");val manager=owner.getSystemService(android.content.Context.PRINT_SERVICE) as android.print.PrintManager;manager.print(view.title ?: "Web page",view.createPrintDocumentAdapter(view.title ?: "Web page"),null)}
      "stop"->{WebHost.pages[key]?.view?.stopLoading()}

      "new_tab"->{val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";WebHost.page(profile,next).view.loadUrl("about:blank")}
      "open"->{val url=command.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");val next="$profile-${java.util.UUID.randomUUID().toString().replace("-", "")}";WebHost.page(profile,next).view.let{navigate(owner,next,it,url)}}
      "select"->{if(key==null||!WebHost.pages.containsKey(key))throw IllegalArgumentException("Browser page closed");select(key)}
      "close"->{if(key!=null)WebHost.close(key)}
      "navigate","back","forward","reload"->{
        val view=WebHost.pages[key]?.view ?: throw IllegalArgumentException("Select a browser page")
        receipts.clear()
        configure(owner,view)
        when(action){"navigate"->{val url=command.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");navigate(owner,key!!,view,url)};"back"->view.goBack();"forward"->view.goForward();else->view.reload()}
      }
      else->throw IllegalArgumentException("Unsupported browser control")
    }
    return JSONObject().put("pages",pages(profile)).put("selected",selected?.removePrefix("$profile-") ?: JSONObject.NULL)
  }
  private val zooms=mutableMapOf<String,Double>()
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
  private data class Receipt(val id:String,val signature:String,val width:Int,val height:Int)
  private val receipts=mutableMapOf<String,Receipt>()
  private val jobs=mutableMapOf<String,String>()
  private val loaded=mutableMapOf<String,()->Unit>()
  private val handler=Handler(Looper.getMainLooper())
  fun invalidate(key:String) { receipts.remove(key) }
  private fun pageInfo(key:String,profile:String,page:WebHost.Page)=JSONObject().put("tab",key.removePrefix("$profile-")).put("url",requestedUrls[key] ?: page.view.url ?: "about:blank").put("title",if(page.view.url=="about:blank")"" else page.view.title ?: "").put("can_back",page.view.canGoBack()).put("can_forward",page.view.canGoForward()).put("zoom",zooms[key] ?: 1.0).put("loading",requestedUrls.containsKey(key)||page.view.progress<100).put("progress",page.view.progress).put("favicon",icons[key] ?: JSONObject.NULL)
  private fun pages(profile:String)=JSONArray().also { result -> WebHost.pages.filter { (key,page)->key.startsWith("$profile-")&&(requestedUrls.containsKey(key)||page.view.url==null||page.view.url=="about:blank"||WebHost.allowed(page.view.url ?: "")) }.entries.sortedBy{tabOrder.indexOf(it.key)}.forEach { (key,page)->result.put(pageInfo(key,profile,page)) } }
  private fun shortcut(code:Int,event:KeyEvent):String?{
    val command=event.isCtrlPressed||event.isMetaPressed;val shift=event.isShiftPressed
    if(command&&!event.isAltPressed){
      if(code==KeyEvent.KEYCODE_TAB)return if(shift)"previous-tab" else "next-tab"
      if(code==KeyEvent.KEYCODE_G)return if(shift)"find-previous" else "find-next"
      if(code==KeyEvent.KEYCODE_EQUALS||code==KeyEvent.KEYCODE_PLUS||code==KeyEvent.KEYCODE_NUMPAD_ADD)return "zoom-in"
      if(shift)return null
      return when(code){
        KeyEvent.KEYCODE_PAGE_UP->"previous-tab";KeyEvent.KEYCODE_PAGE_DOWN->"next-tab"
        KeyEvent.KEYCODE_MINUS,KeyEvent.KEYCODE_NUMPAD_SUBTRACT->"zoom-out";KeyEvent.KEYCODE_0->"zoom-reset"
        in KeyEvent.KEYCODE_1..KeyEvent.KEYCODE_9->"tab-${code-KeyEvent.KEYCODE_0}"
        KeyEvent.KEYCODE_L->"l";KeyEvent.KEYCODE_T->"t";KeyEvent.KEYCODE_W->"w";KeyEvent.KEYCODE_H->"h";KeyEvent.KEYCODE_J->"j";KeyEvent.KEYCODE_F->"f";KeyEvent.KEYCODE_P->"p";KeyEvent.KEYCODE_R->"r";else->null
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
  private fun signature(view:WebView,value:JSONObject)="${view.url}:${view.width}:${view.height}:$value"
  fun request(owner:MainActivity,profile:String,id:String,command:JSONObject,script:String,cursor:String):String {
    cursorScript=cursor
    val result=CompletableFuture<String>()
    val tab=command.optString("tab")
    val key="$profile-$tab"
    fun fail(message:String) { if(jobs[key]==id){jobs.remove(key);loaded.remove(key);invalidate(key)};result.complete(response(id,error=message)) }
    fun finish(observation:JSONObject?=null){if(jobs[key]==id){jobs.remove(key);loaded.remove(key)};result.complete(response(id,JSONObject().put("pages",pages(profile)).put("observation",observation ?: JSONObject.NULL)))}
    fun captureFrame() {
      if(result.isDone||jobs[key]!=id)return
      val page=WebHost.pages[key] ?: run {fail("Browser page closed");return}
      val view=page.view
        select(key)
      invalidate(key)
      metrics(view,script,{before ->
        if(result.isDone||jobs[key]!=id)return@metrics
        val width=view.width;val height=view.height;val prior=signature(view,before)
        if(width<=0||height<=0||width.toLong()*height>8_000_000){fail("Browser viewport exceeds image budget");return@metrics}
        val bitmap=Bitmap.createBitmap(width,height,Bitmap.Config.ARGB_8888)
        fun copied(success:Boolean) {
          cursor(view,JSONObject().put("kind","resume"))
          if(!success){bitmap.recycle();fail("Browser screenshot unavailable");return}
          metrics(view,script,{after ->
            try {
              if(result.isDone||jobs[key]!=id)return@metrics
              if(prior!=signature(view,after))throw IllegalStateException("Browser changed during capture; observe again")
              val bytes=ByteArrayOutputStream().also {bitmap.compress(Bitmap.CompressFormat.PNG,100,it)}.toByteArray()
              if(bytes.size>7*1024*1024)throw IllegalStateException("Browser screenshot exceeds its budget")
              receipts[key]=Receipt(id,prior,width,height)
              finish(JSONObject().put("page",pageInfo(key,profile,page)).put("state_id",id).put("width",width).put("height",height).put("png_base64",Base64.encodeToString(bytes,Base64.NO_WRAP)))
            }catch(error:Exception){fail(error.message ?: "Browser capture failed")}finally{bitmap.recycle()}
          },{message->bitmap.recycle();fail(message)})
        }
        if(Build.VERSION.SDK_INT>=26&&embedded&&(content as? PageFrame)?.cuts.isNullOrEmpty()){
          val location=IntArray(2);view.getLocationInWindow(location)
          val window=owner.window
          if(window==null){copied(false)}else{PixelCopy.request(window,Rect(location[0],location[1],location[0]+width,location[1]+height),bitmap,{status->copied(status==PixelCopy.SUCCESS)},handler)}
        }else{view.draw(Canvas(bitmap));copied(true)}
      },::fail)
    }
    fun capture(){
      val view=WebHost.pages[key]?.view ?: run{fail("Browser page closed");return}
      cursor(view,JSONObject().put("kind","prepare")){cursor(view,JSONObject().put("kind","suspend")){captureFrame()}}
    }
    fun client(page:WebHost.Page) {
      page.view.webViewClient=object:WebViewClient(){
        override fun shouldOverrideUrlLoading(view:WebView,request:WebResourceRequest)=!WebHost.allowed(request.url.toString())
        override fun onPageStarted(view:WebView,url:String,favicon:Bitmap?){invalidate(key);changed()}
        override fun onPageFinished(view:WebView,url:String){finished(owner,key,view,url);if(!WebHost.allowed(url)){fail("Browser URL is not allowed");return};loaded.remove(key)?.invoke()}
        override fun onReceivedSslError(view:WebView,handler:SslErrorHandler,error:SslError){handler.cancel();invalidate(key);if(jobs[key]==id)fail("Browser certificate validation failed")}
        override fun onReceivedError(view:WebView,request:WebResourceRequest,error:WebResourceError){if(request.isForMainFrame){invalidate(key);if(jobs[key]==id)fail("Browser navigation failed")}}
      }
    }
    owner.runOnUiThread {
      if(result.isDone)return@runOnUiThread
      try {
        if(!owner.lifecycle.currentState.isAtLeast(Lifecycle.State.RESUMED))throw IllegalStateException("Open the native client to operate its browser")
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
            val receipt=receipts.remove(key) ?: throw IllegalStateException("Observe before browser input")
            if(receipt.id!=command.getString("state_id"))throw IllegalArgumentException("Stale browser state; observe again")
            metrics(page.view,script,{value ->
              try {
                if(result.isDone||jobs[key]!=id)return@metrics
                if(receipt.signature!=signature(page.view,value))throw IllegalStateException("Browser changed; observe again")
                val input=command.getJSONObject("input");val kind=input.getString("kind");if(kind in listOf("navigate","back","forward","reload"))configure(owner,page.view)
                fun point():Pair<Float,Float>{val x=input.getDouble("x");val y=input.getDouble("y");if(!x.isFinite()||!y.isFinite()||x<0||y<0||x>=receipt.width||y>=receipt.height)throw IllegalArgumentException("Point outside observed browser image");return Pair((x/receipt.width*page.view.width).toFloat(),(y/receipt.height*page.view.height).toFloat())}
                client(page)
                val hint=JSONObject().put("kind",kind)
                when(kind){
                  "move","click","scroll","drag"->{point();hint.put("x",input.getDouble("x")/receipt.width*value.getDouble("width")).put("y",input.getDouble("y")/receipt.height*value.getDouble("height"))}
                }
                if(kind=="click")hint.put("count",input.getInt("count"))
                if(kind=="scroll")hint.put("delta_x",input.getDouble("delta_x")).put("delta_y",input.getDouble("delta_y"))
                if(kind in listOf("navigate","back","forward","reload"))hint.put("kind","hide")
                if(kind=="drag"){hint.put("to_x",input.getDouble("to_x")/receipt.width*value.getDouble("width")).put("to_y",input.getDouble("to_y")/receipt.height*value.getDouble("height"))}
                cursor(page.view,hint){
                metrics(page.view,script,latestMetrics@{latest ->
                try{
                  if(result.isDone||jobs[key]!=id)return@latestMetrics
                  if(receipt.signature!=signature(page.view,latest)){cursor(page.view,JSONObject().put("kind","hide"));throw IllegalStateException("Browser changed before input; observe again")}
                  when(kind){
                  "move"->{val(x,y)=point();mouse(page.view,MotionEvent.ACTION_HOVER_MOVE,x,y,0)}
                  "drag"->{val(x,y)=point();val toX=input.getDouble("to_x");val toY=input.getDouble("to_y");if(!toX.isFinite()||!toY.isFinite()||toX<0||toY<0||toX>=receipt.width||toY>=receipt.height)throw IllegalArgumentException("Drag endpoint outside image");val endX=(toX/receipt.width*page.view.width).toFloat();val endY=(toY/receipt.height*page.view.height).toFloat();mouse(page.view,MotionEvent.ACTION_DOWN,x,y,MotionEvent.BUTTON_PRIMARY);for(step in 1..12){val t=step/12f;mouse(page.view,MotionEvent.ACTION_MOVE,x+(endX-x)*t,y+(endY-y)*t,MotionEvent.BUTTON_PRIMARY)};mouse(page.view,MotionEvent.ACTION_UP,endX,endY,0)}
                  "click"->{val(x,y)=point();repeat(input.getInt("count")){mouse(page.view,MotionEvent.ACTION_DOWN,x,y,when(input.getString("button")){"right"->MotionEvent.BUTTON_SECONDARY;"middle"->MotionEvent.BUTTON_TERTIARY;else->MotionEvent.BUTTON_PRIMARY});mouse(page.view,MotionEvent.ACTION_UP,x,y,0)}}
                  "scroll"->{val(x,y)=point();mouse(page.view,MotionEvent.ACTION_SCROLL,x,y,0,(-input.getDouble("delta_x")/48).toFloat(),(-input.getDouble("delta_y")/48).toFloat())}
                  "type"->{val connection=page.view.onCreateInputConnection(EditorInfo());if(connection==null||!connection.commitText(input.getString("text"),1))throw IllegalStateException("Focus an editable field before typing")}
                  "key"->{val code=keyCode(input.getString("key"));val modifiers=input.getJSONArray("modifiers");var meta=0;for(i in 0 until modifiers.length())meta=meta or when(modifiers.getString(i)){"control"->KeyEvent.META_CTRL_ON;"alt"->KeyEvent.META_ALT_ON;"shift"->KeyEvent.META_SHIFT_ON;"meta"->KeyEvent.META_META_ON;else->throw IllegalArgumentException("Unsupported modifier")};val now=SystemClock.uptimeMillis();page.view.dispatchKeyEvent(KeyEvent(now,now,KeyEvent.ACTION_DOWN,code,0,meta));page.view.dispatchKeyEvent(KeyEvent(now,now,KeyEvent.ACTION_UP,code,0,meta))}
                  "navigate"->{val url=input.getString("url");if(!WebHost.allowed(url))throw IllegalArgumentException("Browser URL not allowed");loaded[key]={handler.postDelayed({capture()},180)};navigate(owner,key,page.view,url)}
                  "back"->{if(!page.view.canGoBack())throw IllegalStateException("No previous browser page");loaded[key]={handler.postDelayed({capture()},180)};page.view.goBack()}
                  "forward"->{if(!page.view.canGoForward())throw IllegalStateException("No next browser page");loaded[key]={handler.postDelayed({capture()},180)};page.view.goForward()}
                  "reload"->{loaded[key]={handler.postDelayed({capture()},180)};page.view.reload()}
                  else->throw IllegalArgumentException("Unsupported browser input")
                }
                if(!loaded.containsKey(key))handler.postDelayed({capture()},250)
                }catch(error:Exception){fail(error.message ?: "Browser input outcome unknown; observe again")}
                },::fail)
                }
              }catch(error:Exception){fail(error.message ?: "Browser input outcome unknown; observe again")}
            },::fail)
          }
          else->throw IllegalArgumentException("Unknown browser operation")
        }
      }catch(error:Exception){fail(error.message ?: "Browser operation failed")}
    }
    return try{result.get(27,TimeUnit.SECONDS)}catch(_:Exception){result.complete(response(id,error="Browser outcome unknown; observe before continuing"));owner.runOnUiThread{if(jobs[key]==id){jobs.remove(key);loaded.remove(key);invalidate(key)}};response(id,error="Browser outcome unknown; do not replay input")}
  }
  private fun cursor(view:WebView,event:JSONObject,done:(()->Unit)?=null){
    if(cursorScript.isEmpty()){done?.invoke();return}
    try{view.evaluateJavascript("($cursorScript)($event)"){done?.invoke()}}catch(_:Exception){done?.invoke()}
  }
  private fun mouse(view:WebView,action:Int,x:Float,y:Float,buttons:Int,horizontal:Float=0f,vertical:Float=0f){
    val properties=MotionEvent.PointerProperties().apply{id=0;toolType=MotionEvent.TOOL_TYPE_MOUSE}
    val coords=MotionEvent.PointerCoords().apply{this.x=x;this.y=y;setAxisValue(MotionEvent.AXIS_HSCROLL,horizontal);setAxisValue(MotionEvent.AXIS_VSCROLL,vertical)}
    val now=SystemClock.uptimeMillis();val event=MotionEvent.obtain(now,now,action,1,arrayOf(properties),arrayOf(coords),0,buttons,1f,1f,0,0,InputDevice.SOURCE_MOUSE,0)
    try{if(action==MotionEvent.ACTION_SCROLL||action==MotionEvent.ACTION_HOVER_MOVE)view.dispatchGenericMotionEvent(event)else view.dispatchTouchEvent(event)}finally{event.recycle()}
  }
  private fun keyCode(key:String):Int=when(key){
    "Enter"->KeyEvent.KEYCODE_ENTER;"Tab"->KeyEvent.KEYCODE_TAB;"Escape"->KeyEvent.KEYCODE_ESCAPE;"Backspace"->KeyEvent.KEYCODE_DEL;"Delete"->KeyEvent.KEYCODE_FORWARD_DEL
    "ArrowUp"->KeyEvent.KEYCODE_DPAD_UP;"ArrowDown"->KeyEvent.KEYCODE_DPAD_DOWN;"ArrowLeft"->KeyEvent.KEYCODE_DPAD_LEFT;"ArrowRight"->KeyEvent.KEYCODE_DPAD_RIGHT
    "Home"->KeyEvent.KEYCODE_MOVE_HOME;"End"->KeyEvent.KEYCODE_MOVE_END;"PageUp"->KeyEvent.KEYCODE_PAGE_UP;"PageDown"->KeyEvent.KEYCODE_PAGE_DOWN;"Space"->KeyEvent.KEYCODE_SPACE
    "F3"->KeyEvent.KEYCODE_F3;"F5"->KeyEvent.KEYCODE_F5;"+"->KeyEvent.KEYCODE_PLUS;"="->KeyEvent.KEYCODE_EQUALS;"-"->KeyEvent.KEYCODE_MINUS
    else->if(key.length==1&&key[0].isLetter()&&key[0].code<128)KeyEvent.KEYCODE_A+(key[0].lowercaseChar()-'a')else if(key.length==1&&key[0].isDigit())KeyEvent.KEYCODE_0+(key[0]-'0')else throw IllegalArgumentException("Unsupported browser key")
  }
}
