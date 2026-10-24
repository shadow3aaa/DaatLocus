"""Opt-in native browser download/history contract against an isolated local site."""
from http.server import BaseHTTPRequestHandler,ThreadingHTTPServer
from pathlib import Path
import json,subprocess,sys,tempfile,threading,time

CONTENT=b"Native browser download: complete original bytes.\n"
class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path=="/file":
            self.send_response(200);self.send_header("Content-Type","application/octet-stream");self.send_header("Content-Disposition",'attachment; filename="example.txt"');data=CONTENT
        else:
            self.send_response(200);self.send_header("Content-Type","text/html");data=b"<!doctype html><title>Download fixture</title><a href='/file' style='position:absolute;left:40px;top:40px;width:200px;height:60px'>Download</a>"
        self.send_header("Content-Length",str(len(data)));self.end_headers();self.wfile.write(data)
    def log_message(self,*args):pass
def main():
    server=ThreadingHTTPServer(("127.0.0.1",0),Fixture);thread=threading.Thread(target=server.serve_forever,daemon=True);thread.start()
    temporary=tempfile.TemporaryDirectory(prefix="dl-download-contract-")
    try:
        root=Path(temporary.name);tab="a"*32;output=root/"result.json";manifest=root/"manifest.json"
        steps=[{"action":"open","tab":tab,"url":f"http://127.0.0.1:{server.server_port}/"},{"action":"input","tab":tab,"state_id":"latest","input":{"kind":"click","x":100/1100,"y":64/800,"button":"left","count":1}},{"action":"observe","tab":tab}]
        manifest.write_text(json.dumps({"output":str(output),"browser_steps":steps,"ui_review":True,"relative_points":True,"hidden_browser":True,"delays":{"2":2500}}),encoding="utf-8")
        subprocess.run([str(Path(sys.argv[1]).resolve(strict=True)),"--web-probe",str(manifest)],check=True,timeout=60)
        results=json.loads(output.read_text(encoding="utf-8"));assert all(value["reply"]["error"] is None for value in results)
        library=results[-1]["library"];assert library["history"][0]["title"]=="Download fixture"
        assert len(library["downloads"])==1;entry=library["downloads"][0]
        assert entry["status"]=="complete",entry;assert entry["name"]=="example.txt";assert entry["bytes"]==len(CONTENT)
        assert (root/"runtime/browser-downloads"/entry["id"]/entry["name"]).read_bytes()==CONTENT
        print("Native browser history and managed download bytes passed.")
    finally:
        server.shutdown();server.server_close();thread.join()
        for attempt in range(30):
            try:temporary.cleanup();break
            except OSError:
                if attempt==29:raise
                time.sleep(.2)
if __name__=="__main__":main()
