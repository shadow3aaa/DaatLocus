"""Opt-in native find and clipboard screenshot test against an isolated local page.

The screenshot case replaces the clipboard with the fixture page image, just as
the Owner screenshot command does. No pre-existing clipboard content is read.
"""
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from io import BytesIO
from PIL import Image
import base64, json, subprocess, sys, tempfile, threading, time

class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        data=b"<!doctype html><style>html,body{margin:0;background:rgb(30,180,80)}p{margin:40px}</style><title>Page actions</title><p>needle</p><p>needle</p><p>needle</p>"
        self.send_response(200); self.send_header("Content-Type","text/html"); self.send_header("Content-Length",str(len(data))); self.end_headers(); self.wfile.write(data)
    def log_message(self,*args): pass

server=ThreadingHTTPServer(("127.0.0.1",0),Fixture)
thread=threading.Thread(target=server.serve_forever,daemon=True); thread.start()
temporary=tempfile.TemporaryDirectory(prefix="dl-page-actions-")
try:
    root=Path(temporary.name); output=root/"result.json"; manifest=root/"manifest.json"; tab="a"*32
    queries=[["find","needle"],["find_next","needle"],["find_previous","needle"],["find","absent-93872"],["find","needle"],["find",""],["find","needle"],["find_close",""]]
    url=f"http://127.0.0.1:{server.server_port}/";second="b"*32;third="c"*32
    steps=[{"action":"open","tab":tab,"url":url},{"action":"observe","tab":tab},{"action":"open","tab":second,"url":url},{"action":"open","tab":third,"url":url},{"action":"observe","tab":third},{"action":"close","tab":third},{"action":"close","tab":second}]
    data={"output":str(output),"hidden_browser":True,"browser_steps":steps,"owner_find":{"1":queries},"owner_screenshots":[1],"panel_steps":{"1":{"x":0,"y":0,"width":1100,"height":800,"scale":1},"2":{"x":80,"y":120,"width":1100,"height":800,"scale":1}},"owner_occlusions":{"1":[{"x":20,"y":20,"width":200,"height":200,"radius":12}],"2":[]},"owner_moves":{"4":[[third,tab]]}}
    manifest.write_text(json.dumps(data),encoding="utf-8")
    subprocess.run([str(Path(sys.argv[1]).resolve(strict=True)),"--web-probe",str(manifest)],check=True,timeout=50)
    results=json.loads(output.read_text(encoding="utf-8")); assert all(value["reply"]["error"] is None for value in results)
    matches=[value["Ok"] for value in results[1]["owner_find"]]
    assert [(v["count"],v["index"]) for v in matches[:3]]==[(3,1),(3,2),(3,1)],matches
    assert matches[3]=={"found":False,"count":0,"index":0},matches[3]
    assert matches[5]=={"found":False,"count":0,"index":0},matches[5]
    assert matches[7]=={"found":False,"count":0,"index":0},matches[7]
    captured=results[1]["screenshot"]; assert captured["copied"]=={"Ok":None},captured
    image=Image.open(BytesIO(base64.b64decode(captured["png"]))).convert("RGB")
    assert image.getpixel((0,0))==(30,180,80),"Screenshot included browser chrome or incorrect pixels"
    assert captured["clipboard"]["width"]==image.width and captured["clipboard"]["height"]==image.height
    assert captured["clipboard"]["pixel"]==[30,180,80,255],"Clipboard did not contain the captured PNG pixels"
    assert results[1]["layer"]=={"has_region":True,"cut_visible":False,"page_visible":True,"position":[0,0]},results[1]["layer"]
    assert not results[2]["layer"]["has_region"],"Native overlay clipping did not clear"
    assert results[2]["layer"]["position"]==[80,120],"Resizing the reparented page changed its requested position"
    assert image.getpixel((100,100))==(30,180,80),"Page capture leaked browser chrome through the native cutout"
    assert [page["tab"] for page in results[4]["owner_workspace"]["pages"]]==[third,tab,second],"Native tab order did not change"
    assert results[4]["owner_workspace"]["selected"]==third,"Reordering changed the selected tab"
    assert results[5]["owner_workspace"]["selected"]==tab,"Closing the active native tab did not select its right neighbor"
    assert results[6]["owner_workspace"]["selected"]==tab,"Closing a background tab changed the selected tab"
    print("Native find: count, next/previous, query replacement, empty/close; screenshot pixels and clipboard image passed.")
finally:
    server.shutdown(); server.server_close(); thread.join()
    for attempt in range(30):
        try: temporary.cleanup(); break
        except OSError:
            if attempt==29: raise
            time.sleep(.2)
