(() => {
  // Capture the whole loaded document, not a depth-limited accessibility preview.
  // Scripts and password values are not reading material. Open shadow roots and
  // accessible frames are collected too; inaccessible surfaces are explicit.
  try {
    const notices = [];
    const visited = new Set();
    function copy(root) {
      const clone = root.cloneNode(true);
      if (root.nodeType !== Node.ELEMENT_NODE) return clone;
      const originals = [root, ...root.querySelectorAll('*')];
      const copies = [clone, ...clone.querySelectorAll('*')];
      for (let i = 0; i < originals.length; i++) {
        const source = originals[i], target = copies[i];
        if (source.shadowRoot) {
          const section = document.createElement('section');
          section.setAttribute('aria-label', 'Shadow content');
          for (const child of source.shadowRoot.childNodes) section.append(copy(child));
          target.append(section);
        }
        if (source.tagName === 'IFRAME' || source.tagName === 'FRAME') {
          try {
            const frame = source.contentDocument;
            if (!frame || !frame.documentElement) throw new Error('inaccessible');
            if (!visited.has(frame)) {
              visited.add(frame);
              const section = document.createElement('section');
              section.setAttribute('aria-label', 'Frame content');
              section.append(copy(frame.body || frame.documentElement));
              target.replaceWith(section);
            }
          } catch (_) {
            notices.push('Frame content was not accessible: ' + (source.getAttribute('src') || '(no URL)'));
          }
        }
        if (source.tagName === 'CANVAS') notices.push('Canvas content requires visual observation: ' + (source.getAttribute('aria-label') || '(unlabelled canvas)'));
        // Resolve using the original document, including its base URL and frames.
        if (source.tagName === 'A' && source.hasAttribute('href')) target.setAttribute('href', source.href);
        if (target.matches('input[type=password]')) target.removeAttribute('value');
      }
      for (const node of clone.querySelectorAll('script,style,noscript')) node.remove();
      if (clone.matches('script,style,noscript')) return document.createComment('');
      return clone;
    }
    visited.add(document);
    if (!document.documentElement) throw new Error('Document is not available');
    const html = copy(document.documentElement).outerHTML;
    if (new TextEncoder().encode(html).length > 8 * 1024 * 1024) throw new Error('Document exceeds the capture budget; no partial snapshot was substituted');
    if (document.readyState !== 'complete') notices.push('The page is still loading; this capture contains the currently loaded document');
    return {url: location.href, title: document.title, html, notices};
  } catch (error) {
    return {error: String(error && error.message || error)};
  }
})()
