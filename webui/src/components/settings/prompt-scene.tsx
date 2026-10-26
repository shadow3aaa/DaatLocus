import { useEffect, useRef, type RefObject } from "react";
import * as THREE from "three";
import type { PromptFile } from "@/lib/system-prompt";
import { promptConnector } from "@/lib/prompt-connector";
import { t } from "@/lib/i18n";

/** The source visualization, with a finite animation loop and explicit GPU cleanup. */
export default function PromptScene({ files, selected, onSelect, onUnavailable, connection, target }: {
  files: PromptFile[]; selected: number; onSelect(index: number): void; onUnavailable(): void;
  connection: RefObject<SVGPathElement | null>; target: RefObject<HTMLSpanElement | null>;
}) {
  const host = useRef<HTMLDivElement>(null), canvas = useRef<HTMLCanvasElement>(null);
  const state = useRef({ selected, onSelect, onUnavailable });
  state.current = { selected, onSelect, onUnavailable };
  const update = useRef<() => void>(() => {});
  useEffect(() => update.current(), [selected]);
  useEffect(() => {
    const element = host.current!, surface = canvas.current!;
    let renderer: THREE.WebGLRenderer;
    try { renderer = new THREE.WebGLRenderer({ canvas: surface, antialias: true, alpha: true }); }
    catch { state.current.onUnavailable(); return; }
    renderer.setPixelRatio(Math.min(devicePixelRatio, 2));
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    renderer.shadowMap.enabled = true;
    renderer.shadowMap.type = THREE.PCFShadowMap;
    const scene = new THREE.Scene(), camera = new THREE.OrthographicCamera(-5, 5, 5, -5, .1, 100);
    camera.position.set(8, 7.2, 10); camera.lookAt(0, 0, 0);
    scene.add(new THREE.AmbientLight("#ffffff", 2));
    const light = new THREE.DirectionalLight("#ffffff", 3.1);
    light.position.set(3, 9, 5); light.castShadow = true;
    light.shadow.mapSize.set(1024, 1024);
    Object.assign(light.shadow.camera, { left: -7, right: 7, top: 7, bottom: -7 });
    light.shadow.normalBias = .025; light.shadow.bias = -.00005; light.shadow.radius = 5;
    scene.add(light);
    const floor = new THREE.Mesh(new THREE.PlaneGeometry(200, 200), new THREE.ShadowMaterial({ color: "#777777", opacity: .055 }));
    floor.rotation.x = -Math.PI / 2; floor.position.y = -1.9; floor.receiveShadow = true; scene.add(floor);
    const group = new THREE.Group(); group.position.y = .12; scene.add(group);
    // Keep a variable file count within the original five-layer envelope.
    const spacing = Math.min(.54, 2.7 / Math.max(1, files.length - 1));
    const meshes = files.map((_, index) => {
      const color = new THREE.Color();
      const materials = [.92, .95, 1, 1, .9, .93].map(shade => new THREE.MeshStandardMaterial({ color: color.clone().multiplyScalar(shade), roughness: .8 }));
      const mesh = new THREE.Mesh(new THREE.BoxGeometry(4.5, .085, 3.55), materials);
      mesh.position.y = ((files.length - 1) / 2 - index) * spacing;
      mesh.castShadow = true; mesh.receiveShadow = true; mesh.userData.index = index;
      mesh.add(new THREE.LineSegments(new THREE.EdgesGeometry(mesh.geometry), new THREE.LineBasicMaterial({ color: color.clone().multiplyScalar(.78), transparent: true, opacity: .32 })));
      group.add(mesh); return mesh;
    });
    const raycaster = new THREE.Raycaster(), pointer = new THREE.Vector2(), anchor = new THREE.Vector3();
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    let hover = -1, frame = 0, last = 0, visible = true, lost = false;
    function draw(now: number) {
      frame = 0;
      if (document.hidden || !visible || lost) return;
      const dt = Math.min((now - last) / 1000, .05); last = now;
      const ease = reduced.matches ? 1 : 1 - Math.exp(-dt * 11);
      let moving = false;
      meshes.forEach((mesh, index) => {
        const x = index === state.current.selected ? .16 : 0;
        const y = ((files.length - 1) / 2 - index) * spacing + (index === hover ? .045 : 0);
        moving ||= Math.abs(x - mesh.position.x) + Math.abs(y - mesh.position.y) > .0001;
        mesh.position.x += (x - mesh.position.x) * ease; mesh.position.y += (y - mesh.position.y) * ease;
        (mesh.children[0] as THREE.LineSegments<THREE.EdgesGeometry, THREE.LineBasicMaterial>).material.opacity = index === state.current.selected ? .55 : .25;
      });
      renderer.render(scene, camera);
      const mesh = meshes[state.current.selected];
      if (mesh && connection.current && target.current) {
        const sceneBounds = element.getBoundingClientRect(), bounds = connection.current.ownerSVGElement!.getBoundingClientRect(), end = target.current.getBoundingClientRect();
        const stacked = end.top >= sceneBounds.bottom - 8;
        const project = (page: typeof mesh, x: number, y: number, z: number) => {
          anchor.set(x, y, z); page.localToWorld(anchor); anchor.project(camera);
          return { x: sceneBounds.left - bounds.left + (anchor.x + 1) * sceneBounds.width / 2, y: sceneBounds.top - bounds.top + (-anchor.y + 1) * sceneBounds.height / 2 };
        };
        const source = project(mesh, 2.25, .045, 0), normal = project(mesh, 3.25, .045, 0);
        const length = Math.hypot(normal.x - source.x, normal.y - source.y);
        const lead = { x: source.x + (normal.x - source.x) / length * 14, y: source.y + (normal.y - source.y) / length * 14 };
        const stack = { right: 0, bottom: 0 };
        for (const page of meshes) for (const x of [-2.25, 2.25]) for (const z of [-1.775, 1.775]) {
          const point = project(page, x, -.045, z);
          stack.right = Math.max(stack.right, point.x); stack.bottom = Math.max(stack.bottom, point.y);
        }
        connection.current.setAttribute("d", promptConnector(source, lead, { x: end.left + end.width / 2 - bounds.left, y: end.top + end.height / 2 - bounds.top }, stack, stacked));
      }
      if (moving) frame = requestAnimationFrame(draw);
    }
    function invalidate() { if (!frame) { last = performance.now(); frame = requestAnimationFrame(draw); } }
    update.current = invalidate;
    // Let the browser resolve shadcn CSS colors (including oklch/color-mix),
    // then give Three.js the same sRGB values as the adjacent file swatches.
    const pixel = document.createElement("canvas"); pixel.width = pixel.height = 1;
    const paint = pixel.getContext("2d", { willReadFrequently: true });
    function palette() {
      const swatches = element.parentElement!.querySelectorAll<HTMLElement>(".dl-prompt-layer-buttons .dl-prompt-swatch");
      if (paint) meshes.forEach((mesh, index) => {
        paint.clearRect(0, 0, 1, 1); paint.fillStyle = getComputedStyle(swatches[index]).backgroundColor; paint.fillRect(0, 0, 1, 1);
        const [r, g, b] = paint.getImageData(0, 0, 1, 1).data;
        const color = new THREE.Color().setRGB(r / 255, g / 255, b / 255, THREE.SRGBColorSpace);
        mesh.material.forEach((material, side) => material.color.copy(color).multiplyScalar([.92, .95, 1, 1, .9, .93][side]));
        (mesh.children[0] as THREE.LineSegments<THREE.EdgesGeometry, THREE.LineBasicMaterial>).material.color.copy(color).multiplyScalar(.78);
      });
      invalidate();
    }
    const theme = new MutationObserver(palette); theme.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style"] });
    palette();
    function resize() {
      const width = element.clientWidth, height = element.clientHeight;
      if (!width || !height) return;
      renderer.setSize(width, height, false);
      const aspect = width / height, span = Math.max(aspect < .8 ? 7.9 : 7.2, aspect * 6.2);
      camera.left = -span / 2; camera.right = span / 2; camera.top = span / aspect / 2; camera.bottom = -span / aspect / 2;
      camera.updateProjectionMatrix(); invalidate();
    }
    function hit(event: PointerEvent) {
      const rect = surface.getBoundingClientRect();
      pointer.set((event.clientX - rect.left) / rect.width * 2 - 1, -(event.clientY - rect.top) / rect.height * 2 + 1);
      raycaster.setFromCamera(pointer, camera);
      return raycaster.intersectObjects(meshes, false)[0]?.object.userData.index ?? -1;
    }
    const move = (event: PointerEvent) => { hover = hit(event); surface.style.cursor = hover < 0 ? "default" : "pointer"; invalidate(); };
    const leave = () => { hover = -1; invalidate(); };
    const click = (event: PointerEvent) => { const index = hit(event); if (index >= 0) state.current.onSelect(index); };
    const contextLost = (event: Event) => { event.preventDefault(); lost = true; state.current.onUnavailable(); };
    surface.addEventListener("pointermove", move); surface.addEventListener("pointerleave", leave); surface.addEventListener("pointerup", click);
    surface.addEventListener("webglcontextlost", contextLost);
    document.addEventListener("visibilitychange", invalidate);
    reduced.addEventListener("change", invalidate);
    const observer = new ResizeObserver(resize); observer.observe(element);
    if (target.current) observer.observe(target.current.parentElement!);
    const intersection = new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; invalidate(); }); intersection.observe(element);
    resize();
    return () => {
      update.current = () => {}; cancelAnimationFrame(frame); observer.disconnect(); intersection.disconnect(); theme.disconnect();
      surface.removeEventListener("pointermove", move); surface.removeEventListener("pointerleave", leave); surface.removeEventListener("pointerup", click);
      surface.removeEventListener("webglcontextlost", contextLost); document.removeEventListener("visibilitychange", invalidate); reduced.removeEventListener("change", invalidate);
      scene.traverse(object => { if (object instanceof THREE.Mesh || object instanceof THREE.LineSegments) { object.geometry.dispose(); const materials = Array.isArray(object.material) ? object.material : [object.material]; materials.forEach(material => material.dispose()); } });
      light.shadow.map?.dispose(); renderer.dispose();
    };
  }, [files, connection, target]);
  return <div className="dl-prompt-canvas" ref={host}>
    <canvas ref={canvas} aria-label={t("context_composition.diagram")} />
  </div>;
}
