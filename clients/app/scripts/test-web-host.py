"""Opt-in integration probe of the installed native WebView, without an account.

Build `cargo build -p daat-locus-app --features web-contract` first, then run
`python clients/app/scripts/test-web-host.py target/debug/daat-locus-app.exe`.
"""
import json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time


class Fixture(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/nested/frame":
            body = "<base href='/frame-base/'><p>FRAME_TEXT</p><a href='next'>Frame destination</a>"
        elif self.path == "/inspect":
            body = "<p id='cookie'></p><script>document.querySelector('#cookie').textContent='COOKIE:'+document.cookie+':END'</script>"
        else:
            body = """<title>Native capture fixture</title><main>FIRST_MAIN</main>
            <div id='shadow'></div><iframe src='/nested/frame'></iframe>
            <input type='password' value='SECRET_PASSWORD'><canvas></canvas><p id='seed-cookie'></p>
            <script>
            document.cookie='profile_secret=SEED; path=/';
            document.querySelector('#seed-cookie').textContent='SEED_COOKIE:'+document.cookie;
            document.querySelector('#shadow').attachShadow({mode:'open'}).innerHTML=
              'SHADOW_DIRECT<p>SHADOW_PARAGRAPH</p><a href="/shadow-next">Shadow link</a>';
            const deep=document.createElement('div'); deep.textContent='DEEP_CONTENT';
            let outer=deep; for(let i=0;i<20;i++){const node=document.createElement('div');node.append(outer);outer=node;} document.body.append(outer);
            const SECRET_SCRIPT='not reading material';
            </script><main>SECOND_MAIN</main>"""
        data = ("<!doctype html><html><body>" + body + "</body></html>").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


def main():
    executable = Path(sys.argv[1]).resolve(strict=True)
    server = ThreadingHTTPServer(("127.0.0.1", 0), Fixture)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        temporary = tempfile.TemporaryDirectory(prefix="dl-web-contract-")
        try:
            directory = temporary.name
            root = Path(directory)
            origin = f"http://127.0.0.1:{server.server_port}"
            output = root / "captures.json"
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"urls": [origin + "/capture-fixture", origin + "/inspect"], "output": str(output)}))
            subprocess.run([str(executable), "--web-probe", str(manifest)], check=True, timeout=80)
            results = json.loads(output.read_text(encoding="utf-8"))
            assert len(results) == 2
            for result in results:
                assert result["reply"]["error"] is None, result["reply"]["error"]
            capture = results[0]["reply"]["capture"]
            html = capture["html"]
            for fact in ["FIRST_MAIN", "SECOND_MAIN", "SHADOW_DIRECT", "SHADOW_PARAGRAPH", "FRAME_TEXT", "DEEP_CONTENT"]:
                assert fact in html, f"Native capture lost {fact}"
            assert origin + "/frame-base/next" in html, "Frame link resolved against the wrong document"
            assert origin + "/shadow-next" in html
            assert "SECRET_PASSWORD" not in html
            assert "SECRET_SCRIPT" not in html
            assert "SEED_COOKIE:profile_secret=SEED" in html, "The cookie isolation fixture did not initialize"
            assert any("Canvas" in notice for notice in capture["notices"])
            assert "COOKIE::END" in results[1]["reply"]["capture"]["html"], "Native profiles leaked site cookies"
            print("Native WebView capture and profile-isolation checks passed.")
        finally:
            # WebView2 may release profile files shortly after its event loop exits.
            for attempt in range(30):
                try:
                    temporary.cleanup()
                    break
                except OSError:
                    if attempt == 29:
                        raise
                    time.sleep(0.2)
    finally:
        server.shutdown()
        server.server_close()
        thread.join()


if __name__ == "__main__":
    main()
