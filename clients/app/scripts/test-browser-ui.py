"""Opt-in UI exercise through the project's native BrowserUse, against the mock server.

Start DAAT_LOCUS_MOCK=1 Vite on 5177 first. Screenshots go only to a supplied
artifact directory; test profiles are isolated and cleaned after WebView exits.
"""
import base64,json,subprocess,sys,tempfile,time
from pathlib import Path

EXE=Path(sys.argv[1]).resolve(strict=True)
ARTIFACTS=Path(sys.argv[2]).resolve();ARTIFACTS.mkdir(parents=True,exist_ok=True)
BASE="http://127.0.0.1:5177/browser"
TAB="a"*32

def probe(name,steps,targets=None,narrow=False,delays=None,downloaded=False,sizes=None):
    temporary=tempfile.TemporaryDirectory(prefix="dl-browser-ui-")
    try:
        root=Path(temporary.name);output=root/"result.json";manifest=root/"manifest.json"
        data={"output":str(output),"browser_steps":steps,"relative_points":True,"hidden_browser":True,"ui_review":True,"ui_targets":targets or {},"delays":{"1":700}}
        if delays:data["delays"].update(delays)
        if narrow:data["panel_steps"]={"0":{"x":0,"y":0,"width":390,"height":844,"scale":1}}
        if sizes:data["panel_steps"]={str(index):{"x":0,"y":0,"width":width,"height":height,"scale":1} for index,(width,height) in sizes.items()}
        manifest.write_text(json.dumps(data),encoding="utf-8")
        try:subprocess.run([str(EXE),"--web-probe",str(manifest)],check=True,timeout=45)
        except Exception:
            if output.exists():print(output.read_text(encoding="utf-8")[:2000])
            raise
        results=json.loads(output.read_text(encoding="utf-8"))
        for index,value in enumerate(results):
            assert value["reply"]["error"] is None,(index,value["reply"]["error"])
            observation=value["reply"]["browser"].get("observation")
            if observation:(ARTIFACTS/f"{name}-{index}.png").write_bytes(base64.b64decode(observation["png_base64"]))
            ui=value.get("ui")
            if ui:assert ui["scrollWidth"]<=ui["width"],f"{name}: horizontal page overflow"
        if downloaded:
            entries=[entry for entry in results[-1]["library"]["downloads"] if entry["name"].endswith(".png")]
            assert len(entries)==1 and entries[0]["status"]=="complete",entries
            entry=entries[0];data=(root/"runtime"/"browser-downloads"/entry["id"]/entry["name"]).read_bytes()
            assert data.startswith(b"\x89PNG\r\n\x1a\n") and len(data)>1000,"Mobile screenshot did not download a PNG image"
            (ARTIFACTS/f"{name}-download.png").write_bytes(data)
        return results
    finally:
        for attempt in range(30):
            try:temporary.cleanup();break
            except OSError:
                if attempt==29:raise
                time.sleep(.2)

def opened(url=BASE):return {"action":"open","tab":TAB,"url":url}
def observe():return {"action":"observe","tab":TAB}
def action(kind,**values):return {"action":"input","tab":TAB,"state_id":"latest","input":{"kind":kind,**values}}
def click():return action("click",x=0,y=0,button="left",count=1)
def key(value,modifiers=None):return action("key",key=value,modifiers=modifiers or [])

initial=probe("home",[opened(),observe()])
locale=initial[-1]["ui"]["language"]
copy=json.loads((Path("webui/src/locales")/("zh-CN.json" if locale.startswith("zh") else "en-US.json")).read_text(encoding="utf-8"))["browser_use"]
assert "GitHub" in initial[-1]["ui"]["text"]
steps=[opened(),observe(),click(),click(),click(),click(),action("type",text="MDN"),key("A",["control"]),action("type",text="no-such-visit-734"),click(),click(),click(),action("type",text="browser interface"),key("Enter"),click()]
targets={"2":"aria:"+copy["menu"],"3":"menu:"+copy["downloads"],"4":"aria:"+copy["history"],"5":"aria:"+copy["search_history"],"9":"aria:"+copy["back"],"10":"aria:"+copy["new_tab"],"11":"aria:"+copy["address"],"14":"aria:"+copy["close_tab"].replace("{{title}}","www.google.com")}
results=probe("desktop",steps,targets,delays={"10":400})
assert "browser-guide.txt" in results[3]["ui"]["text"]
assert "MDN Web Docs" in results[6]["ui"]["text"] and "GitHub" not in results[6]["ui"]["text"],"History search did not filter the visible list"
assert "MDN Web Docs" not in results[8]["ui"]["text"]
assert any("https://www.google.com/search?q=browser%20interface"==value["value"] for value in results[13]["ui"]["inputs"]),"Omnibox did not navigate to the search result"
assert sum(button["label"].startswith(copy["close_tab"].split("{{title}}")[0]) for button in results[14]["ui"]["buttons"])==2,"Closing the new tab lost an existing tab"
probe("narrow",[opened(),observe(),click(),click()],{"2":"aria:"+copy["menu"],"3":"menu:"+copy["downloads"]},narrow=True)
probe("empty",[opened(BASE+"?browser_scene=empty"),observe()])
# These points come from the inspected 1100x800 fixture screenshot. Exercise the
# opaque sandboxed iframe through native pointer input, without reading its DOM.
cursor_steps=[opened(BASE+"?browser_scene=cursor&motion=full"),observe()]
for x in [220,289,358,427,497,566,650,737,805]:cursor_steps.append(action("click",x=x/1100,y=294/800,button="left",count=1))
cursor_steps.append(action("click",x=199/1100,y=353/800,button="left",count=1))
cursor_steps.append(action("click",x=289/1100,y=294/800,button="left",count=1))
probe("cursor",cursor_steps)
probe("cursor-reduced",[opened(BASE+"?browser_scene=cursor&motion=reduce"),observe(),action("click",x=289/1100,y=294/800,button="left",count=1)])
find_steps=[opened(BASE+"?browser_scene=cursor"),observe(),key("F",["control"]),action("type",text="光标"),observe(),key("Enter"),key("Enter",["shift"]),key("A",["control"]),action("type",text="missing-29817"),observe(),key("Escape"),observe()]
find_results=probe("find",find_steps,delays={"4":500,"9":500})
assert any(value["value"]=="光标" for value in find_results[4]["ui"]["inputs"]),"Find shortcut did not focus the page search field"
assert "0 / 0" in find_results[9]["ui"]["text"],"Missing page text was not reported"
assert not any(value["label"]==copy["find"] for value in find_results[-1]["ui"]["inputs"]),"Escape did not close page search"
assert "1 / 3" in find_results[4]["ui"]["text"] and "2 / 3" in find_results[5]["ui"]["text"] and "1 / 3" in find_results[6]["ui"]["text"],"Find controls did not move between actual matches"
probe("narrow-find",find_steps,narrow=True,delays={"4":500,"9":500})
capture_results=probe("page-screenshot",[opened(BASE+"?browser_scene=cursor"),observe(),click(),click(),observe()],{"2":"aria:"+copy["menu"],"3":"menu:"+copy["screenshot"]},delays={"4":2500})
assert copy["screenshot_clipboard"] in capture_results[-1]["ui"]["text"],"Mock page screenshot did not reach the clipboard"
mobile_capture=probe("mobile-screenshot",[opened(BASE+"?browser_scene=cursor"),observe(),click(),click(),observe()],{"2":"aria:"+copy["menu"],"3":"menu:"+copy["screenshot"]},narrow=True,delays={"4":1500},downloaded=True)
assert copy["screenshot_download"] in mobile_capture[-1]["ui"]["text"],"Mobile screenshot did not report its saved image"
print("Browser mock: navigation, library, tabs, desktop/mobile find, clipboard screenshots and mobile PNG download passed.")

drag=action("drag",x=150/1100,y=20/800,to_x=440/1100,to_y=20/800)
middle=action("click",x=0,y=0,button="middle",count=1)
tabs=probe("tab-drag",[opened(),observe(),drag,observe(),middle,observe()],{"4":"tab:光标与输入演示"},delays={"3":1300})
assert [tab["text"] for tab in tabs[3]["ui"]["tabs"]]==["光标与输入演示",copy["new_tab"]],"Dragging did not reorder tabs across a refresh"
assert len(tabs[-1]["ui"]["tabs"])==1 and tabs[-1]["ui"]["tabs"][0]["selected"],"Middle-close did not preserve the selected background tab"
closed=probe("middle-close",[opened(BASE+"?browser_scene=cursor"),observe(),middle],{"2":"tab:光标与输入演示"})
assert len(closed[-1]["ui"]["tabs"])==1 and closed[-1]["ui"]["tabs"][0]["selected"],"Closing the active tab did not select an adjacent tab"
narrow=probe("narrow-no-drag",[opened(),observe(),action("drag",x=90/390,y=20/844,to_x=290/390,to_y=20/844),observe()],narrow=True)
assert not any(tab["draggable"] for tab in narrow[-1]["ui"]["tabs"]),"Narrow touch layout enabled tab dragging"
assert [tab["text"] for tab in narrow[-1]["ui"]["tabs"]]==[copy["new_tab"],"光标与输入演示"],"Narrow layout reordered a tab"
keys=[opened(BASE+"?browser_scene=cursor"),observe(),key("T",["control"]),key("1",["control"]),key("9",["control"]),key("Tab",["control","shift"]),key("PageDown",["control"]),key("W",["control"]),key("F",["control"]),action("type",text="光标"),observe(),key("F3"),key("F3",["shift"]),key("L",["control"]),action("type",text="https://example.org/"),key("Enter"),key("F5"),observe(),key("ArrowLeft",["alt"]),key("ArrowRight",["alt"])]
keyboard=probe("shortcuts",keys,delays={"10":500,"17":800})
def selected(index):return next(tab["text"] for tab in keyboard[index]["ui"]["tabs"] if tab["selected"])
assert len(keyboard[2]["ui"]["tabs"])==3 and selected(3)==copy["new_tab"] and selected(5)=="光标与输入演示", "New-tab, numbered tab or reverse cycling failed"
assert len(keyboard[7]["ui"]["tabs"])==2 and selected(7)=="光标与输入演示", "Close shortcut did not select the adjacent tab"
assert "2 / 3" in keyboard[11]["ui"]["text"] and "1 / 3" in keyboard[12]["ui"]["text"], "F3 match navigation failed"
assert any(value["value"]=="https://example.org/" for value in keyboard[17]["ui"]["inputs"]) and len(keyboard[17]["ui"]["tabs"])==2,"F5 refreshed the surrounding application instead of the current web page"
assert any(value["value"]=="https://example.com/browser-demo" for value in keyboard[18]["ui"]["inputs"]),"Alt+Left did not go back"
assert any(value["value"]=="https://example.org/" for value in keyboard[19]["ui"]["inputs"]),"Alt+Right did not go forward"
print("Browser tab gestures and keyboard shortcuts passed, including narrow drag prevention.")
for native,narrow in [(False,False),(True,False),(True,True)]:
    url=BASE+"?browser_scene=cursor"+("&native_clip=1" if native else "")
    overlay=probe("overlay-mobile" if narrow else "overlay-native" if native else "overlay-live",[opened(url),observe(),click(),observe(),key("Escape"),observe()],{"2":"aria:"+copy["menu"]},narrow=narrow,delays={"3":600,"5":300})
    assert overlay[3]["ui"]["surface"]["live"],"Opening the menu blanked the current page"
    if native:assert overlay[3]["ui"]["surface"]["clipped"],"Native-style overlay did not apply its shape"
    assert overlay[5]["ui"]["surface"]["live"] and not overlay[5]["ui"]["surface"]["clipped"],"Closing the menu did not restore the live page"
print("Browser menu retains the live page and restores the page after closing.")

# Popovers are real portals, retain the live page, and own their scroll region.
downloads=probe("downloads-popup",[opened(BASE+"?browser_scene=downloads&native_clip=1"),observe(),key("J",["control"]),observe(),click(),click(),observe(),action("scroll",x=900/1100,y=400/800,delta_x=0,delta_y=600),observe(),key("Escape"),observe()],{"4":"aria:"+copy["download_actions"].replace("{{name}}","unnamed.png"),"5":"menu:"+copy["remove_download"]},delays={"3":400,"6":300,"8":200,"10":200})
assert downloads[3]["ui"]["surface"]=={"live":True,"clipped":True},"Downloads hid the native-style page"
popup=downloads[3]["ui"]["downloadPopup"]
assert popup and popup["x"]>=0 and popup["x"]+popup["width"]<=downloads[3]["ui"]["width"] and popup["y"]+popup["height"]<=downloads[3]["ui"]["height"],popup
assert downloads[6]["ui"]["downloadPopup"] and "unnamed.png" not in downloads[6]["ui"]["text"],"Removing a record closed the popup or retained the record"
assert downloads[8]["ui"]["downloadPopup"]["scrollTop"]>0,"Download list did not scroll inside the popup"
assert downloads[10]["ui"]["downloadPopup"] is None and downloads[10]["ui"]["surface"]=={"live":True,"clipped":False},"Escape did not dismiss the download popup"
narrow=probe("downloads-fullpage",[opened(BASE+"?browser_scene=downloads"),observe(),key("J",["control"]),observe(),click(),observe()],{"4":"aria:"+copy["back"]},narrow=True,delays={"3":300,"5":300})
assert narrow[3]["ui"]["downloadPopup"] is None and not narrow[3]["ui"]["surface"]["live"],"Narrow downloads did not use the full-page view"
assert narrow[5]["ui"]["surface"]["live"],"Back did not restore the browser page"
responsive=probe("downloads-resize",[opened(BASE+"?browser_scene=downloads"),observe(),key("J",["control"]),observe(),observe(),observe(),observe(),observe()],sizes={0:(1100,800),4:(390,844),6:(1100,800)},delays={"5":300,"7":300})
assert responsive[3]["ui"]["downloadPopup"] and responsive[5]["ui"]["downloadPopup"] is None and not responsive[5]["ui"]["surface"]["live"],"Shrinking the window did not retain downloads in the full-page view"
assert responsive[7]["ui"]["downloadPopup"] and responsive[7]["ui"]["surface"]["live"],"Widening the window did not restore the download popup"
print("Download popup: native-style clipping, bounded scrolling, nested record actions, Escape, narrow Back navigation and responsive transitions passed.")

entries=probe("downloads-entry",[opened(BASE+"?browser_scene=downloads&native_clip=1"),observe(),click(),observe(),click(),observe(),click(),click(),observe()],{"2":"aria:"+copy["downloads"],"4":"aria:"+copy["downloads"],"6":"aria:"+copy["menu"],"7":"menu:"+copy["downloads"]},delays={"3":250,"5":200,"7":200,"8":350})
assert entries[3]["ui"]["downloadPopup"] and entries[5]["ui"]["downloadPopup"] is None and entries[8]["ui"]["downloadPopup"],"Downloads button toggle or menu entry failed"
print("Downloads toolbar toggle and More-menu focus handoff passed.")
