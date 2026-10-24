(() => {
  const key = Symbol.for('daat-locus.browser-observation');
  if (!window[key]) {
    const state = { revision: 0 };
    const changed = () => state.revision++;
    new MutationObserver(changed).observe(document, {subtree:true, childList:true, attributes:true, characterData:true});
    for (const event of ['input','change','scroll','pointerdown','pointermove','keydown']) document.addEventListener(event, changed, true);
    window[key] = state;
  }
  return {url:location.href, width:innerWidth, height:innerHeight, x:scrollX, y:scrollY, dpr:devicePixelRatio, revision:window[key].revision, ready:document.readyState};
})()
