// Runs in WebKit's isolated client world in the focused frame. No input dispatch.
({action,key,shift,meta,control,marker,token}) => {
  const id=Symbol.for('daat-locus.browser-key-default');
  const remove=state=>{
    window.removeEventListener('keydown',state.keydown,true);
    for(const type of ['copy','cut','paste'])window.removeEventListener(type,state.edit,true);
  };
  if(action==='finish') {
    const state=window[id];
    if(!state||state.token!==token)return {delivered:false,prevented:true,defaulted:false};
    return state.done.then(()=>{
      if(state.cancelled||window[id]!==state)return {delivered:false,prevented:true,defaulted:false};
      remove(state);delete window[id];
      return {delivered:Boolean(state.event),prevented:state.event?.defaultPrevented??true,defaulted:state.defaulted};
    });
  }
  const old=window[id];
  if(old){remove(old);old.cancelled=true;old.resolve();}
  const state={token,event:null,defaulted:false,cancelled:false};
  state.done=new Promise(resolve=>state.resolve=resolve);
  state.keydown=event=>{
    if(!state.event&&event.isTrusted&&event.metaKey===meta&&event.ctrlKey===control&&!event.altKey&&event.shiftKey===shift&&event.key.toLowerCase()===key){
      state.event=event;
      // A new task observes defaultPrevented after every event handler and
      // synchronous native editing default, including handlers in page JS.
      setTimeout(()=>state.resolve(),0);
    }
  };
  state.edit=event=>{
    if(state.event&&event.isTrusted&&event.type===marker)state.defaulted=true;
  };
  window.addEventListener('keydown',state.keydown,true);
  for(const type of ['copy','cut','paste'])window.addEventListener(type,state.edit,true);
  window[id]=state;return {};
}
