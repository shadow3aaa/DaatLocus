// Read page paint only. This detached canvas never changes the page or adds a bridge.
(() => {
  try {
    const canvas = document.createElement("canvas");
    canvas.width = canvas.height = 1;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) return null;
    context.fillStyle = "Canvas";
    context.fillRect(0, 0, 1, 1);
    for (const node of [document.documentElement, document.body]) {
      if (!node) continue;
      context.fillStyle = getComputedStyle(node).backgroundColor;
      context.fillRect(0, 0, 1, 1);
    }
    return "#" + [...context.getImageData(0, 0, 1, 1).data].slice(0, 3)
      .map(channel => channel.toString(16).padStart(2, "0")).join("");
  } catch {
    return null;
  }
})()
