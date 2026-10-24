import type { BrowserHitRegion } from "./browser-workspace";

// Geometry routes native input only. Rendering stays in the system compositor:
// CSS opacity, shadows and animation never become masks on the webpage.
export function browserForeground(viewport: HTMLElement): BrowserHitRegion[] {
  const area = viewport.getBoundingClientRect();
  const regions: BrowserHitRegion[] = [];
  const styles = new Map<Element, CSSStyleDeclaration>();
  const boxes = new Map<Element, DOMRect>();
  const styleOf = (node:Element) => { let style=styles.get(node); if(!style){style=getComputedStyle(node);styles.set(node,style);}return style; };
  const boxOf = (node:Element) => { let box=boxes.get(node);if(!box){box=node.getBoundingClientRect();boxes.set(node,box);}return box; };
  const covers=(a:BrowserHitRegion,b:BrowserHitRegion)=>a.x<=b.x&&a.y<=b.y&&a.x+a.width>=b.x+b.width&&a.y+a.height>=b.y+b.height;
  let covered=false;
  function add(region:BrowserHitRegion){
    if(regions.some(other=>covers(other,region)))return;
    for(let i=regions.length-1;i>=0;i--)if(covers(region,regions[i]))regions.splice(i,1);
    regions.push(region);
    covered=covers(region,{x:area.x,y:area.y,width:area.width,height:area.height,radius:0});
  }
  function visit(node: Element) {
    if (covered) return;
    if (node instanceof HTMLIFrameElement && node.parentElement === viewport) return;
    const style = styleOf(node);
    if (style.display === "none" || style.contentVisibility === "hidden") return;
    if (node !== viewport && !node.contains(viewport) && style.visibility !== "hidden" && style.pointerEvents !== "none") {
      for (const box of node.getClientRects()) {
        let left = Math.max(box.left, area.left), top = Math.max(box.top, area.top);
        let right = Math.min(box.right, area.right), bottom = Math.min(box.bottom, area.bottom);
        for (let parent = node.parentElement; parent && parent !== document.body; parent = parent.parentElement) {
          const clip = styleOf(parent), rect = boxOf(parent);
          if (/hidden|clip|auto|scroll/.test(clip.overflowX)) { left = Math.max(left, rect.left); right = Math.min(right, rect.right); }
          if (/hidden|clip|auto|scroll/.test(clip.overflowY)) { top = Math.max(top, rect.top); bottom = Math.min(bottom, rect.bottom); }
        }
        if (right > left && bottom > top) add({x:left,y:top,width:right-left,height:bottom-top,radius:0});
      }
    }
    // Portals usually come last. A full modal hit surface makes traversing the
    // conversation beneath it unnecessary, even when that history is long.
    for (let index=node.children.length-1;index>=0&&!covered;index--) visit(node.children[index]);
  }
  visit(document.body);
  return regions;
}
