"""Real native BrowserUse contract against local pages; never uses a personal account."""
import base64
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from io import BytesIO
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
from urllib.parse import parse_qs, urlsplit
from PIL import Image, ImageChops

requests = []
class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        requests.append(self.path)
        if self.path.startswith("/done"):
            body="<title>Submitted</title><h1>Submitted</h1>"
        elif self.path.startswith("/shared-inspect"):
            body="<script>document.title='Shared:'+document.cookie</script>"
        elif self.path.startswith("/second"):
            body="<title>Second</title><div style='height:2400px;background:linear-gradient(white,blue)'>Second page</div><script>onscroll=()=>document.title='Scrolled:'+Math.round(scrollY)</script>"
        else:
            body="""<title>Browser fixture</title><style>body{margin:0;height:2400px;background:#fff}button,input{position:absolute;left:40px;box-sizing:border-box}#click{top:40px;width:160px;height:48px;background:#f00}input{top:110px;width:300px;height:44px}#submit{top:180px;width:160px;height:48px}</style>
            <div id='hover' style='position:absolute;left:400px;top:40px;width:240px;height:48px;background:#00f'>Hover</div><div id='drag' style='position:absolute;left:400px;top:110px;width:240px;height:44px;background:#ccc;touch-action:none'>Drag</div><button id='click' type='button'>Count</button><form action='/done'><input name='q' id='text'><button id='submit'>Submit</button></form>
            <script>window.nativeControlResult='unavailable';if(window.__TAURI_INTERNALS__){window.__TAURI_INTERNALS__.invoke('browser_control',{action:'list',tab:null,url:null,bounds:null}).then(()=>window.nativeControlResult='allowed',()=>window.nativeControlResult='denied')}document.cookie='shared_browser=present; path=/';document.querySelector('#hover').onmouseenter=e=>document.title='Hovered:'+e.isTrusted;const drag=document.querySelector('#drag');drag.onpointerdown=e=>drag.setPointerCapture(e.pointerId);drag.onpointerup=e=>document.title='Dragged:'+e.isTrusted+':'+Math.round(e.clientX);let clicks=0;document.querySelector('#click').onclick=e=>{clicks++;document.title='Clicked:'+clicks+':'+e.isTrusted;e.target.style.background='#00ff00'};
            document.querySelector('#text').oninput=e=>document.title='Typed:'+e.target.value+':'+e.isTrusted;
            document.querySelector('#text').onkeydown=e=>{if(e.ctrlKey&&e.key.toLowerCase()==='a')document.title='SelectAll:'+e.isTrusted};</script>"""
        if self.path.startswith("/browser"):
            body=body.replace("window.nativeControlResult=", "const nativeMatch=matchMedia.bind(window);window.matchMedia=query=>query==='(prefers-reduced-motion: reduce)'?{matches:"+str(self.server.reduced_motion).lower()+"}:nativeMatch(query);window.nativeControlResult=",1)
        data=("<!doctype html><meta charset='utf-8'><meta name='viewport' content='width=device-width,initial-scale=1'>"+body).encode()
        self.send_response(200);self.send_header("Content-Type","text/html; charset=utf-8");self.send_header("Content-Length",str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args): pass

def main():
    executable=Path(sys.argv[1]).resolve(strict=True)
    server=ThreadingHTTPServer(("127.0.0.1",0),Fixture)
    server.reduced_motion="--reduced-motion" in sys.argv
    thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    temporary=tempfile.TemporaryDirectory(prefix="dl-browser-contract-")
    try:
        root=Path(temporary.name);origin=f"http://127.0.0.1:{server.server_port}";tab="a"*32
        def action(kind,**values): return {"action":"input","tab":tab,"state_id":"latest","input":{"kind":kind,**values}}
        click=action("click",x=100/1100,y=64/800,button="left",count=1)
        stale={**click,"state_id":"first"}
        steps=[{"action":"open","tab":tab,"url":origin+"/browser"},click,stale,{"action":"observe","tab":tab},
            action("click",x=100/1100,y=130/800,button="left",count=1),action("type",text="原生 browser🙂"),
            action("key",key="A",modifiers=["control"]),action("type",text="替换文本"),action("key",key="Enter",modifiers=[]),
            action("navigate",url=origin+"/second"),action("back"),action("forward"),action("reload"),
            action("scroll",x=500/1100,y=400/800,delta_x=0,delta_y=600),action("navigate",url=origin+"/browser"),action("move",x=420/1100,y=64/800),action("drag",x=420/1100,y=130/800,to_x=620/1100,to_y=130/800),{"action":"observe","tab":tab},action("click",x=100/1100,y=64/800,button="left",count=1),{"action":"list"},{"action":"observe","tab":tab},action("click",x=100/1100,y=64/800,button="left",count=1),{"action":"observe","tab":tab},action("click",x=100/1100,y=64/800,button="left",count=1),{"action":"open","tab":"b"*32,"url":origin+"/shared-inspect"},{"action":"list"},{"action":"observe","tab":tab},{"action":"close","tab":tab},{"action":"list"},{"action":"close","tab":"b"*32},{"action":"list"}]
        output=root/"result.json";manifest=root/"manifest.json"
        data={"output":str(output),"browser_steps":steps,"relative_points":True,"cursor_frames":[1],"hidden_browser":"--hidden" in sys.argv,"owner_steps":{"18":{"kind":"key","key":"Tab","modifiers":[]},"21":{"kind":"key","key":"Escape","modifiers":[]}}}
        if "--embedded" in sys.argv:data["panel_steps"]={"0":{"x":80,"y":120,"width":1100,"height":800,"scale":1},"26":None}
        manifest.write_text(json.dumps(data),encoding="utf-8")
        subprocess.run([str(executable),"--web-probe",str(manifest)],check=True,timeout=150)
        results=json.loads(output.read_text(encoding="utf-8"))
        assert len(results)==len(steps)
        for index,result in enumerate(results):
            error=result["reply"]["error"]
            if index in [2,18,21]: assert error is not None,"Stale browser state after concurrent Owner input was accepted"
            else: assert error is None,f"Step {index} {steps[index]}: {error}"
        def observation(index):return results[index]["reply"]["browser"]["observation"]
        assert (results[0].get("native_control") or {}).get("status") in ["denied","unavailable"],"Third-party page can invoke Owner browser controls"
        if "--embedded" in sys.argv:
            assert results[0]["parent"]=="main","Browser page was not embedded in the content window"
            assert results[26]["parent"]=="browser","Leaving the browser route did not detach the native page"
        assert observation(1)["page"]["title"]=="Clicked:1:true","Click was simulated, replayed or missed its target"
        assert observation(3)["page"]["title"]=="Clicked:1:true","Rejected stale input still clicked"
        assert observation(5)["page"]["title"]=="Typed:原生 browser🙂:true","Unicode text did not reach the focused browser field through trusted input"
        assert observation(6)["page"]["title"]=="SelectAll:true","Modified key did not reach the browser"
        assert observation(7)["page"]["title"]=="Typed:替换文本:true"
        assert parse_qs(urlsplit(observation(8)["page"]["url"]).query)["q"]==["替换文本"],"Enter did not submit the actual form"
        assert observation(10)["page"]["title"]=="Submitted"
        assert observation(11)["page"]["title"]=="Second"
        assert observation(13)["page"]["title"].startswith("Scrolled:")
        assert int(observation(13)["page"]["title"].split(":")[1])>0
        assert observation(15)["page"]["title"]=="Hovered:true"
        assert observation(16)["page"]["title"]=="Dragged:true:620"
        assert observation(20)["page"]["title"]=="Dragged:true:620","Model input used state invalidated by Owner input"
        assert observation(22)["page"]["title"]=="Dragged:true:620","Concurrent Owner input reused the old observation"
        assert observation(23)["page"]["title"]=="Clicked:1:true","Fresh model input after Owner input did not work"
        assert observation(24)["page"]["title"]=="Shared:shared_browser=present","Shared browser tabs did not reuse site cookies"
        assert [page["tab"] for page in results[25]["reply"]["browser"]["pages"]]==[tab,"b"*32],"Shared tabs are missing or reorder on every list"
        assert [page["tab"] for page in results[28]["reply"]["browser"]["pages"]]==["b"*32],"Closing one tab closed the other shared page"
        for index in [0,1,5,13]:
            view=observation(index);image=Image.open(BytesIO(base64.b64decode(view["png_base64"])));image.load()
            assert image.size==(view["width"],view["height"])
            assert image.width>300 and image.height>200
        view=observation(1);image=Image.open(BytesIO(base64.b64decode(view["png_base64"]))).convert("RGB")
        r,g,b=image.getpixel((round(image.width*60/1100),round(image.height*50/800)))
        assert g>200 and r<50 and b<50,"Screenshot does not show the button's actual changed pixels"
        # Inspect the actual native drawing independently of the clean model image.
        for index,kind,x,y in [(1,"click",100,64),(15,"move",420,64),(16,"drag",620,130)]:
            cursor=results[index]["cursor"]
            assert cursor["state"]["visible"],f"Step {index}: accepted input has no visible cursor"
            assert cursor["state"]["kind"]==kind
            assert abs(cursor["state"]["x"]-x)<1 and abs(cursor["state"]["y"]-y)<1
            decorated=Image.open(BytesIO(base64.b64decode(cursor["png"]))).convert("RGB")
            clean=Image.open(BytesIO(base64.b64decode(observation(index)["png_base64"]))).convert("RGB")
            sx=decorated.width/1100;sy=decorated.height/800
            box=(round(x*sx),round(y*sy),round((x+24)*sx),round((y+30)*sy))
            def pointer_pixels(picture):
                return sum(1 for r,g,b in picture.crop(box).getdata() if max(r,g,b)<65)
            assert pointer_pixels(decorated)>pointer_pixels(clean)+15,"Native cursor shape was not painted over the page"
            if index==1:
                pixel=clean.getpixel((round(108*sx),round(75*sy)))
                assert pixel[1]>200 and pixel[0]<50 and pixel[2]<50,"Cursor leaked into the model's observation screenshot"
        frames=[Image.open(BytesIO(base64.b64decode(value["png"]))).convert("RGB") for value in [results[1]["cursor"],*results[1]["cursor_frames"]]]
        assert len(frames)==3
        scale_x=frames[0].width/1100;scale_y=frames[0].height/800
        ring_box=(round(75*scale_x),round(39*scale_y),round(125*scale_x),round(89*scale_y))
        difference=ImageChops.difference(frames[0].crop(ring_box),frames[-1].crop(ring_box)).getbbox()
        if server.reduced_motion:assert difference is None,"Reduced motion still animated the cursor"
        else:assert difference,"Click feedback did not animate in the native page"
        for index,kind in [(5,"type"),(6,"key")]:
            assert results[index]["cursor"]["state"]["kind"]==kind
        for index in [18,21]:
            assert not results[index]["cursor"]["state"]["visible"],"Owner input did not hide the model cursor"
        assert results[-1]["reply"]["browser"]["pages"]==[]
        assert 1<=len([path for path in requests if path.startswith('/done')])<=2,"Unexpected requests beyond the original submit and optional history revalidation"
        print("Native BrowserUse: trusted input, navigation, painted cursor, clean model screenshots, stale-state refusal, concurrent Owner input fencing, shared cookies/tabs and closure passed.")
    finally:
        server.shutdown();server.server_close();thread.join()
        for attempt in range(30):
            try: temporary.cleanup();break
            except OSError:
                if attempt==29:raise
                time.sleep(0.2)
if __name__=="__main__":main()
