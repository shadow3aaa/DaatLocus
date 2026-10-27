// Client-only decoration. No input dispatch, page-content collection or native bridge.
(event) => {
  const key = Symbol.for('daat-locus.browser-cursor');
  let state = window[key];
  const now = () => performance.now();
  const active = () => Boolean(state && state.active && now() < state.until);
  const visible = () => active() && !state.suspended && state.host.isConnected;
  if (event.kind==='prepare' && state?.host.isConnected) return {visible:visible()};
  if (['hide', 'suspend', 'resume', 'inspect'].includes(event.kind)) {
    if (!state) return {visible:false};
    if (event.kind === 'hide') { state.active=false; clearTimeout(state.timer); }
    if (event.kind === 'suspend') state.suspended=true;
    if (event.kind === 'resume') state.suspended=false;
    state.layer.style.visibility=visible() ? 'visible' : 'hidden';
    return {visible:visible(),kind:state.kind,x:state.x,y:state.y,reduced:state.reduced};
  }
  const style = (node, values) => { Object.assign(node.style, values); return node; };
  const svg = (width, height, paths) => {
    const node=document.createElementNS('http://www.w3.org/2000/svg','svg');
    node.setAttribute('viewBox',`0 0 ${width} ${height}`);
    node.setAttribute('width',String(width));node.setAttribute('height',String(height));
    for (const attributes of paths) {
      const path=document.createElementNS(node.namespaceURI,'path');
      for (const [name,value] of Object.entries(attributes)) path.setAttribute(name,String(value));
      node.append(path);
    }
    return node;
  };
  if (!state || !state.host.isConnected) {
    if (!document.documentElement) return {visible:false};
    const host=style(document.createElement('dl-agent-pointer'),{
      all:'initial',position:'fixed',inset:'0',width:'100vw',height:'100vh',
      pointerEvents:'none',userSelect:'none',overflow:'hidden',zIndex:'2147483647',contain:'strict'
    });
    host.inert=true;host.setAttribute('aria-hidden','true');
    // Keep animation/style mutations outside the page's observation tree.
    const shadow=host.attachShadow({mode:'closed'});
    const layer=style(document.createElement('div'),{position:'absolute',inset:'0',pointerEvents:'none',visibility:'hidden'});
    const arrow=style(document.createElement('div'),{position:'absolute',left:'-4px',top:'-5px',width:'30px',height:'32px',willChange:'transform',pointerEvents:'none'});
    const glow=style(document.createElement('div'),{position:'absolute',left:'-12px',top:'-12px',width:'50px',height:'50px',borderRadius:'50%',background:'rgba(110,155,250,.43)',filter:'blur(12px)',pointerEvents:'none'});
    const pointer=svg(30,32,[{d:'M5 3.6 L26.5 10.8 C29 11.6 29 13.2 26.5 14.1 L18.5 17 C17.7 17.3 17.2 17.8 16.9 18.6 L13.9 26.5 C12.9 29.1 11.2 29.1 10.3 26.5 L3.1 5.2 C2.4 3.2 3.2 3 5 3.6 Z',fill:'#08090b',stroke:'#fff','stroke-width':2.2,'stroke-linejoin':'round'}]);
    style(pointer,{position:'relative',overflow:'visible',filter:'drop-shadow(0 1px 1.5px #0003)'});
    arrow.append(glow,pointer);
    const badge=style(document.createElement('div'),{position:'absolute',display:'flex',alignItems:'center',justifyContent:'center',width:'26px',height:'26px',padding:'0',boxSizing:'border-box',borderRadius:'7px',background:'#202020',color:'#fff',font:'600 12px/26px system-ui,sans-serif',whiteSpace:'nowrap',boxShadow:'0 1px 4px #0004',pointerEvents:'none'});
    const rings=[0,1].map(() => style(document.createElement('div'),{position:'absolute',left:'0',top:'0',width:'16px',height:'16px',margin:'-8px',border:'2px solid #fff',borderRadius:'50%',boxSizing:'border-box',boxShadow:'0 0 0 1px #17171799',opacity:'0',pointerEvents:'none'}));
    const trail=document.createElementNS('http://www.w3.org/2000/svg','svg');
    style(trail,{position:'absolute',inset:'0',width:'100%',height:'100%',overflow:'hidden',pointerEvents:'none',opacity:'0'});
    const line=document.createElementNS(trail.namespaceURI,'path');
    for (const [name,value] of Object.entries({fill:'none',stroke:'#777','stroke-width':'2','stroke-dasharray':'4 4'})) line.setAttribute(name,value);
    trail.append(line);layer.append(trail,...rings,arrow,badge);shadow.append(layer);document.documentElement.append(host);
    state={host,layer,arrow,badge,rings,trail,line,x:20,y:20,active:false,suspended:false,until:0,timer:0,kind:''};
    window[key]=state;
  }
  if(event.kind==='prepare') return {visible:false};
  // Hidden WebViews pause animation timelines. Paint the final position rather
  // than freezing the arrow at the beginning of its movement animation.
  const reduced=document.hidden || window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  state.reduced=reduced;
  const animate=(node,frames,options) => {
    node.getAnimations?.().forEach(animation=>animation.cancel());
    if (reduced || !node.animate) return;
    node.animate(frames,options);
  };
  let x=Number.isFinite(event.x) ? event.x : state.x;
  let y=Number.isFinite(event.y) ? event.y : state.y;
  if (event.kind==='type' || event.kind==='key') {
    let target=document.activeElement;
    while (target?.shadowRoot?.activeElement) target=target.shadowRoot.activeElement;
    if (target && target!==document.body && target!==document.documentElement) {
      const rect=target.getBoundingClientRect();
      if (rect.width>0 && rect.height>0) { x=rect.left+Math.min(20,rect.width/2);y=rect.top+Math.min(18,rect.height/2); }
    }
  }
  x=Math.max(0,Math.min(innerWidth-2,x));y=Math.max(0,Math.min(innerHeight-2,y));
  const start=event.kind==='drag' ? {x,y} : {x:state.x,y:state.y};
  if (event.kind==='drag') {
    const endX=Math.max(0,Math.min(innerWidth-2,event.to_x));
    const endY=Math.max(0,Math.min(innerHeight-2,event.to_y));
    state.line.setAttribute('d',`M${x} ${y} L${endX} ${endY}`);
    state.trail.style.opacity=reduced ? '0.55' : '0';
    animate(state.trail,[{opacity:0.65},{opacity:0}],{duration:900,fill:'forwards'});
    x=endX;y=endY;
  } else {
    state.trail.getAnimations?.().forEach(animation=>animation.cancel());state.trail.style.opacity='0';
  }
  const transform=`translate3d(${x}px,${y}px,0)`;
  state.arrow.style.transform=transform;
  animate(state.arrow,[{transform:`translate3d(${start.x}px,${start.y}px,0)`},{transform}],{duration:event.kind==='drag' ? 240 : 100,easing:'ease-out'});
  state.rings.forEach((ring,index)=>{
    ring.getAnimations?.().forEach(animation=>animation.cancel());ring.style.opacity='0';
    if (event.kind==='click' && index < (event.count || 1) || event.kind==='drag' && index===0) {
      ring.style.left=`${x}px`;ring.style.top=`${y}px`;
      ring.style.opacity=reduced ? '0.9' : '0';
      animate(ring,[{transform:'scale(0.5)',opacity:0.95},{transform:'scale(2)',opacity:0}],{duration:430,delay:index*90,fill:'forwards'});
    }
  });
  state.badge.replaceChildren();state.badge.style.display='none';
  if (event.kind==='type' || event.kind==='key') {
    state.badge.style.display='flex';state.badge.append(svg(18,18,[{d:'M2 4 H16 V14 H2 Z M4 7 H5 M8 7 H9 M12 7 H13 M4 10 H5 M8 10 H9 M12 10 H13 M5 12 H13',fill:'none',stroke:'currentColor','stroke-width':1.5,'stroke-linecap':'round','stroke-linejoin':'round'}]));
  } else if (event.kind==='scroll') {
    const horizontal=Math.abs(event.delta_x)>Math.abs(event.delta_y);
    const forward=horizontal ? event.delta_x>0 : event.delta_y>0;
    const d=horizontal ? (forward ? 'M3 9 H15 M10 4 L15 9 L10 14' : 'M15 9 H3 M8 4 L3 9 L8 14') : (forward ? 'M9 3 V15 M4 10 L9 15 L14 10' : 'M9 15 V3 M4 8 L9 3 L14 8');
    state.badge.style.display='flex';state.badge.append(svg(18,18,[{d,fill:'none',stroke:'currentColor','stroke-width':1.6,'stroke-linecap':'round','stroke-linejoin':'round'}]));
  }
  state.badge.style.left=`${Math.max(4,Math.min(innerWidth-30,x+22))}px`;
  state.badge.style.top=`${Math.max(4,Math.min(innerHeight-30,y+26))}px`;
  const idleTimeout=30_000;
  state.x=x;state.y=y;state.kind=event.kind;state.active=true;state.suspended=false;state.until=now()+idleTimeout;
  state.layer.style.visibility=visible() ? 'visible' : 'hidden';
  clearTimeout(state.timer);state.timer=setTimeout(()=>{state.active=false;state.layer.style.visibility='hidden';},idleTimeout);
  return {visible:visible(),kind:state.kind,x,y};
}
