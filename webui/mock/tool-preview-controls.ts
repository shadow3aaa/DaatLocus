import { examplesForScene, toolScenes } from "./tool-showcase";

const escape = (value: string) => value.replaceAll("&","&amp;").replaceAll('"',"&quot;").replaceAll("<","&lt;").replaceAll(">","&gt;");
/** Mock-only navigation; expansion clicks the real conversation disclosures. */
export function toolPreviewControls(scene: string, selected?: string) {
  const examples = examplesForScene(scene);
  if (!examples.length) return {html:"",script:""};
  const html = `<a href="/__mock/frame?scene=tools-all&width=1000">Combined work record</a><label>Category <select id="tool-group">${toolScenes.map(item=>`<option value="${item.id}"${item.id===scene?" selected":""}>${escape(item.label)}</option>`).join("")}</select></label><label>Example <select id="tool-case"><option value="">All (${examples.length} items)</option>${examples.map(item=>`<option value="${item.id}"${item.id===selected?" selected":""}>${escape(item.label)}</option>`).join("")}</select></label><button id="tool-expand" type="button">${selected ? "Expand tools and details" : "Expand tool group"}</button>`;
  const script = `
    const group=document.getElementById('tool-group'),example=document.getElementById('tool-case'),frame=document.getElementById('preview-frame');
    function selectExample(){const next=new URL(location.href);next.searchParams.set('scene',group.value);next.searchParams.set('width',document.getElementById('preview-width').value);if(group.value==='${scene}'&&example.value)next.searchParams.set('case',example.value);else next.searchParams.delete('case');location.href=next.href;}
    group.addEventListener('change',selectExample);example.addEventListener('change',selectExample);
    let expanding;
    function expandTools(){
      if(expanding)expanding.disconnect();
      const doc=frame.contentDocument;if(!doc||!doc.body)return;
      const open=()=>doc.querySelectorAll('.dl-activity-heading[aria-expanded="false"]${selected ? ',.dl-step-label[aria-expanded="false"]:not([disabled])' : ''}').forEach(button=>button.click());
      const observer=new frame.contentWindow.MutationObserver(open);expanding=observer;observer.observe(doc.body,{childList:true,subtree:true});open();setTimeout(()=>observer.disconnect(),5000);
    }
    document.getElementById('tool-expand').addEventListener('click',expandTools);
    ${selected ? "frame.addEventListener('load',expandTools);" : ""}
  `;
  return {html,script};
}
