package io.shadow3aaa.daatlocus

import android.app.Instrumentation
import android.graphics.BitmapFactory
import android.os.SystemClock
import android.util.Base64
import android.view.MotionEvent
import android.view.inputmethod.EditorInfo
import android.webkit.WebView
import org.json.JSONObject
import org.json.JSONTokener
import java.net.ServerSocket
import java.net.InetAddress
import java.util.UUID
import java.util.concurrent.CompletableFuture
import java.util.concurrent.TimeUnit

/** Real WebViews and native input; no account credentials or external websites. */
internal object BrowserBackgroundTest {
  @JvmStatic fun run(instrumentation:Instrumentation,owner:MainActivity):String {
    val server=ServerSocket(0,8,InetAddress.getByName("127.0.0.1"))
    val serving=Thread {
      while(!server.isClosed) {
        val socket=try{server.accept()}catch(_:Exception){break}
        socket.use {
          val input=it.getInputStream().bufferedReader()
          val request=input.readLine() ?: ""
          while(!input.readLine().isNullOrEmpty()) {}
          val color=if(request.contains("/background"))"rgb(0,200,0)" else "white"
          val body="<!doctype html><style>html,body{margin:0;background:$color;height:2000px}input{position:fixed;left:10%;top:10%;width:60%;height:40px}</style><input><script>window.events=[];for(const kind of ['pointerdown','pointerup','pointermove','mousedown','mouseup','click','auxclick','contextmenu'])document.addEventListener(kind,e=>{events.push({kind,trusted:e.isTrusted,x:e.clientX,y:e.clientY,button:e.button,buttons:e.buttons});if(kind==='contextmenu'||kind==='auxclick')e.preventDefault();});</script>".toByteArray()
          it.getOutputStream().apply{write("HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: ${body.size}\r\nConnection: close\r\n\r\n".toByteArray());write(body);flush()}
        }
      }
    }.apply{isDaemon=true;start()}
    fun <T> ui(block:()->T):T {
      val result=CompletableFuture<T>()
      instrumentation.runOnMainSync {try{result.complete(block())}catch(error:Throwable){result.completeExceptionally(error)}}
      return result.get(10,TimeUnit.SECONDS)
    }
    fun evaluate(view:WebView,script:String):Any {
      val result=CompletableFuture<Any>()
      ui {view.evaluateJavascript(script){result.complete(JSONTokener(it).nextValue())}}
      return result.get(10,TimeUnit.SECONDS)
    }
    val profile=(UUID.randomUUID().toString()+UUID.randomUUID().toString()).replace("-","")
    val a=UUID.randomUUID().toString().replace("-","")
    val b=UUID.randomUUID().toString().replace("-","")
    val frames=mutableMapOf<String,JSONObject>()
    fun control(action:String,tab:String?=null):JSONObject=ui {
      val command=JSONObject().put("profile",profile).put("action",action)
      if(tab!=null)command.put("tab",tab)
      BrowserUse.control(owner,command)
    }
    fun request(command:JSONObject):JSONObject {
      val reply=JSONObject(BrowserUse.request(owner,profile,UUID.randomUUID().toString().replace("-",""),command,"(() => ({width:innerWidth,height:innerHeight}))()","(_event)=>({})"))
      check(reply.isNull("error")){"$command => $reply"}
      val observation=reply.getJSONObject("browser").optJSONObject("observation")
      if(observation!=null)frames[command.getString("tab")]=observation
      return reply
    }
    fun input(tab:String,kind:String,values:JSONObject=JSONObject()) {
      values.put("kind",kind)
      request(JSONObject().put("action","input").put("tab",tab).put("state_id",frames[tab]!!.getString("state_id")).put("input",values))
    }
    fun click(tab:String) {
      val frame=frames[tab]!!
      input(tab,"click",JSONObject().put("x",frame.getInt("width")*.2).put("y",frame.getInt("height")*.11).put("button","left").put("count",1))
    }
    fun view(tab:String)=ui{WebHost.pages["$profile-$tab"]!!.view}
    try {
      val deadline=SystemClock.elapsedRealtime()+30000
      while(ui{WebHost.clientView.get()==null}) {check(SystemClock.elapsedRealtime()<deadline){"Native shell did not start"};SystemClock.sleep(100)}
      request(JSONObject().put("action","open").put("tab",a).put("url","http://127.0.0.1:${server.localPort}/owner"))
      check(control("list").isNull("selected")){"Agent opening a tab selected it"}
      control("select",a)
      ui {
        val shell=WebHost.clientView.get()!!
        BrowserUse.control(owner,JSONObject().put("profile",profile).put("action","mount").put("bounds",JSONObject().put("x",0).put("y",0).put("width",shell.width).put("height",shell.height).put("scale",1)))
      }
      instrumentation.waitForIdleSync()
      val ownerPage=view(a)
      ui {
        ownerPage.requestFocus()
        val now=SystemClock.uptimeMillis()
        for(kind in listOf(MotionEvent.ACTION_DOWN,MotionEvent.ACTION_UP)) {
          val event=MotionEvent.obtain(now,now,kind,ownerPage.width*.2f,ownerPage.height*.11f,0)
          try{ownerPage.dispatchTouchEvent(event)}finally{event.recycle()}
        }
      }
      check(ui{owner.hasWindowFocus()}){"Owner Activity was not focused before background operations"}
      fun unchanged(){
        check(ui{owner.hasWindowFocus()}){"Background operation stole the Owner window's focus"}
        check(control("list").getString("selected")==a){"Agent changed Owner selection"}
        check(ui{owner.currentFocus===ownerPage}){"Agent changed Owner native keyboard focus"}
      }
      fun ownerType(text:String){ui{val current=owner.currentFocus as? WebView ?: error("Owner focus left its WebView");check(current.onCreateInputConnection(EditorInfo())?.commitText(text,1)==true)}}
      request(JSONObject().put("action","open").put("tab",b).put("url","http://127.0.0.1:${server.localPort}/background"));unchanged()
      check(ui{WebHost.pages["$profile-$b"]!!.dialog.window?.decorView?.hasWindowFocus()==false}){"Background native window became focused"}
      click(b);unchanged()
      check(evaluate(view(b),"document.activeElement.tagName")=="INPUT"){
        "Background click did not focus its DOM field: "+evaluate(view(b),"JSON.stringify({active:document.activeElement?.tagName,focus:document.hasFocus(),width:innerWidth,height:innerHeight,rect:document.querySelector('input').getBoundingClientRect().toJSON(),hit:document.elementFromPoint(innerWidth*.2,innerHeight*.11)?.tagName,events:window.events})")+ui{WebHost.pages["$profile-$b"]!!.view.let{" native=${it.width}x${it.height} attached=${it.isAttachedToWindow} focused=${it.hasFocus()}"}}
      }
      ownerType("U")
      input(b,"type",JSONObject().put("text","Agent"));unchanged();ownerType("V")
      input(b,"key",JSONObject().put("key","L").put("modifiers",org.json.JSONArray().put("control")));unchanged();ownerType("W")
      check(evaluate(ownerPage,"document.querySelector('input').value")=="UVW")
      check(evaluate(view(b),"document.querySelector('input').value")=="Agent")
      val png=Base64.decode(frames[b]!!.getString("png_base64"),Base64.NO_WRAP)
      val bitmap=BitmapFactory.decodeByteArray(png,0,png.size)
      check(bitmap.getPixel(0,0)==android.graphics.Color.rgb(0,200,0)){"Background snapshot did not contain the target page"};bitmap.recycle()
      for((button,code) in listOf("right" to 2,"middle" to 1)) {
        val frame=frames[b]!!
        input(b,"click",JSONObject().put("x",frame.getInt("width")*.2).put("y",frame.getInt("height")*.11).put("button",button).put("count",1))
        unchanged()
        check(evaluate(view(b),"events.some(e=>e.kind==='mousedown'&&e.button===$code&&e.trusted)")==true){"Native $button button was not delivered"}
      }
      val dragFrame=frames[b]!!
      input(b,"drag",JSONObject().put("x",dragFrame.getInt("width")*.2).put("y",dragFrame.getInt("height")*.5).put("to_x",dragFrame.getInt("width")*.4).put("to_y",dragFrame.getInt("height")*.5))
      unchanged()
      check(evaluate(view(b),"events.some(e=>e.kind==='pointermove'&&e.buttons===1&&e.trusted)")==true){"Native drag movement was not delivered"}
      val scrollFrame=frames[b]!!
      input(b,"scroll",JSONObject().put("x",scrollFrame.getInt("width")*.5).put("y",scrollFrame.getInt("height")*.5).put("delta_x",0).put("delta_y",96))
      unchanged()
      check((evaluate(view(b),"scrollY") as Number).toDouble()>0){"Background native scroll did not move the page"}
      control("select",b)
      check(evaluate(view(b),"document.querySelector('input').value")=="Agent")
      click(b);input(b,"type",JSONObject().put("text","X"))
      check(evaluate(view(b),"document.querySelector('input').value")=="AgentX")
      control("select",a)
      request(JSONObject().put("action","observe").put("tab",b))
      check(control("list").getString("selected")==a)
      request(JSONObject().put("action","close").put("tab",b))
      check(control("list").getString("selected")==a)
      return "background selection, keyboard isolation, live snapshots and Owner switching passed"
    } finally {
      ui {WebHost.close("$profile-$a");WebHost.close("$profile-$b")}
      server.close();serving.join(2000)
    }
  }
}
