package io.shadow3aaa.daatlocus;

import org.json.JSONArray;
import org.json.JSONObject;
import java.io.*;
import java.net.*;
import java.nio.charset.StandardCharsets;
import java.util.*;

/** Local deterministic model fixture. All GUI facts come from actual App results. */
final class PhoneUseProvider implements AutoCloseable {
    final ServerSocket server;
    volatile String failure, screenshot;
    volatile boolean done, receivedImage;
    private String actor;
    private int coreStep, phoneStep;
    private String lastFacts="";
    private final String text = "Actor → Rust → JNI → Android 🚀";
    private final String stage;
    private final Thread thread;
    PhoneUseProvider(String stage) throws Exception {
        this.stage=stage;
        server = new ServerSocket(0, 10, InetAddress.getByName("127.0.0.1"));
        thread = new Thread(() -> {
            while (!server.isClosed()) {
                try (Socket socket = server.accept()) { serve(socket); }
                catch (Throwable error) { if (!server.isClosed()) failure = error + "; core step="+coreStep+", phone step="+phoneStep+"; "+lastFacts; }
            }
        }, "phone-test-provider");
        thread.setDaemon(true); thread.start();
    }
    private void serve(Socket socket) throws Exception {
        socket.setSoTimeout(10000);
        InputStream input = socket.getInputStream();
        StringBuilder headers = new StringBuilder(); int c;
        while (headers.length()<16384 && (c=input.read())!=-1) {
            headers.append((char)c); if (headers.toString().endsWith("\r\n\r\n")) break;
        }
        int length = 0;
        for (String line:headers.toString().split("\r\n")) if (line.toLowerCase(Locale.ROOT).startsWith("content-length:")) length=Integer.parseInt(line.substring(15).trim());
        if (length<1 || length>4_000_000) throw new IOException("Invalid provider request size");
        JSONObject request = new JSONObject(new String(input.readNBytes(length), StandardCharsets.UTF_8));
        JSONArray tools = request.optJSONArray("tools"), messages=request.getJSONArray("messages");
        if (tools==null) tools=new JSONArray();
        lastFacts="";
        for(int i=Math.max(0,messages.length()-3);i<messages.length();i++) {
            JSONObject m=messages.getJSONObject(i); Object content=m.opt("content");
            String detail=content instanceof String ? ((String)content).substring(0,Math.min(600,((String)content).length())) : content==null?"none":content.getClass().getSimpleName();
            lastFacts+=m.optString("role")+": "+detail+"; ";
        }
        Map<String,String> names = new HashMap<>();
        for(int i=0;i<tools.length();i++) {
            String name=tools.getJSONObject(i).getJSONObject("function").getString("name");
            int separator=name.lastIndexOf("__");
            names.put(separator<0?name:name.substring(separator+2),name);
        }
        JSONObject payload=null;
        for(int i=0;i<messages.length();i++) {
            JSONObject message=messages.getJSONObject(i);
            if("tool".equals(message.optString("role"))) {
                String content=message.optString("content");
                if(content.startsWith("{")) payload=new JSONObject(content);
            }
            JSONArray content=message.optJSONArray("content");
            if(content!=null) for(int j=0;j<content.length();j++) {
                JSONObject part=content.optJSONObject(j);
                if(part!=null && "image_url".equals(part.optString("type"))) {
                    String url=part.getJSONObject("image_url").getString("url");
                    if(url.startsWith("data:image/png;base64,")) {
                        byte[] png=android.util.Base64.decode(url.substring(url.indexOf(',')+1),android.util.Base64.DEFAULT);
                        if(png.length>8 && png[0]==(byte)137 && png[1]==80 && png[2]==78 && png[3]==71) receivedImage=true;
                    }
                }
            }
        }
        String operation=null; JSONObject arguments=null;
        boolean phone=false;
        for(int i=0;i<tools.length();i++) if(tools.getJSONObject(i).getJSONObject("function").getString("name").endsWith("phone_use__list_apps")) phone=true;
        boolean review=names.containsKey("review_app_call") || request.has("response_format") && !phone;
        if(review) {
            // Independent automatic-review request, with no execution tools/history.
            if(names.containsKey("review_app_call")) { operation="review_app_call";arguments=new JSONObject().put("decision","approve").put("reason","Explicit disposable GUI fixture test"); }
        } else if(phone) {
            if(payload!=null && (payload.has("error") || payload.has("observation_error"))) throw new AssertionError("Actual App failed: "+payload);
            switch(phoneStep++) {
                case 0: operation="list_apps";arguments=new JSONObject().put("query","PhoneUse Test Fixture").put("app_id",JSONObject.NULL).put("cursor",JSONObject.NULL).put("limit",100);break;
                case 1: operation="launch_app";arguments=new JSONObject().put("app_id",payload.getJSONArray("apps").getJSONObject(0).getString("app_id"));break;
                case 2: {
                    JSONObject o=payload.getJSONObject("observation"); screenshot=o.getString("screenshot_url");
                    if(!o.getJSONObject("screenshot").has("id")) throw new AssertionError("No actual resource ledger reference");
                    operation="click";arguments=state(o).put("x",640).put("y",96).put("button","left").put("click_count",1);break;
                }
                case 3: operation="set_text";arguments=state(payload.getJSONObject("observation")).put("text",text);break;
                case 4: {
                    JSONObject o=payload.getJSONObject("observation");
                    if(!text.equals(o.getJSONObject("input_target").getString("text"))) throw new AssertionError("Actor text did not reach the real control");
                    operation="close_window";arguments=state(o);break;
                }
                default: if(!receivedImage) throw new AssertionError("Actual model request did not include the screenshot");done=true;
            }
        } else {
            switch(coreStep++) {
                case 0: operation="create";arguments=new JSONObject().put("stage_id",stage).put("name","PhoneUse integration fixture").put("workspace",JSONObject.NULL).put("instructions","Run the deterministic phone fixture test").put("model",JSONObject.NULL);break;
                case 1: actor=payload.getString("id");operation="send";arguments=new JSONObject().put("stage_id",stage).put("actor_id",actor).put("content","Run PhoneUse integration fixture");break;
            }
        }
        JSONObject delta=new JSONObject().put("content",review?"{\"decision\":\"approve\",\"reason\":\"Explicit disposable GUI fixture test\"}":"PhoneUse integration complete");
        if(operation!=null) delta=new JSONObject().put("tool_calls",new JSONArray().put(new JSONObject().put("index",0).put("id","test_"+UUID.randomUUID()).put("type","function").put("function",new JSONObject().put("name",names.get(operation)).put("arguments",arguments.toString()))));
        JSONObject chunk=new JSONObject().put("id","phone-test").put("object","chat.completion.chunk").put("created",1).put("model","phone-test").put("choices",new JSONArray().put(new JSONObject().put("index",0).put("delta",delta).put("finish_reason",JSONObject.NULL)));
        JSONObject finish=new JSONObject(chunk.toString());finish.getJSONArray("choices").getJSONObject(0).put("delta",new JSONObject()).put("finish_reason",operation==null?"stop":"tool_calls");
        boolean stream=request.optBoolean("stream");
        JSONObject ordinary=new JSONObject(chunk.toString()).put("object","chat.completion");
        JSONObject choice=ordinary.getJSONArray("choices").getJSONObject(0);
        choice.remove("delta");choice.put("message",new JSONObject(delta.toString()).put("role","assistant")).put("finish_reason",operation==null?"stop":"tool_calls");
        byte[] body=(stream?"data: "+chunk+"\n\ndata: "+finish+"\n\ndata: [DONE]\n\n":ordinary.toString()).getBytes(StandardCharsets.UTF_8);
        OutputStream output=socket.getOutputStream();
        output.write(("HTTP/1.1 200 OK\r\nContent-Type: "+(stream?"text/event-stream":"application/json")+"\r\nContent-Length: "+body.length+"\r\nConnection: close\r\n\r\n").getBytes(StandardCharsets.US_ASCII));output.write(body);output.flush();
    }
    private static JSONObject state(JSONObject o) throws Exception {return new JSONObject().put("window_id",o.getString("window_id")).put("state_id",o.getString("state_id"));}
    @Override public void close() throws Exception {server.close();thread.join(2000);}
}
