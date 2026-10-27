import { useEffect, useId, useRef, type RefObject } from "react";
import * as THREE from "three";
import { layerColor, type PromptFile } from "@/lib/system-prompt";
import { projectedSurfaceLead, promptConnector } from "@/lib/prompt-connector";
import { layerFrame } from "@/lib/prompt-layer-motion";
import { t } from "@/lib/i18n";

/** The source visualization, with a finite animation loop and explicit GPU cleanup. */
export default function PromptScene({ files, selected, active = true, onSelect, onStep, onUnavailable, connection, target, removal, removeTarget, creation, createTarget }: {
  files: PromptFile[]; selected: number; onSelect(index: number): void; onUnavailable(): void;
  active?: boolean; onStep?(direction: number): void;
  connection: RefObject<SVGPathElement | null>; target: RefObject<HTMLSpanElement | null>;
  removal: RefObject<SVGPathElement | null>; removeTarget: RefObject<HTMLButtonElement | null>;
  creation?: RefObject<SVGPathElement | null>; createTarget?: RefObject<HTMLButtonElement | null>;
}) {
  const listId = useId();
  const host = useRef<HTMLDivElement>(null), canvas = useRef<HTMLCanvasElement>(null);
  const previousFiles = useRef<PromptFile[]>([]);
  const previousPoses = useRef(new Map<string, { y: number; x: number; opacity: number }>());
  const previousExits = useRef<{ file: PromptFile; y: number; x: number; startY: number; targetY: number; initialOpacity: number; phaseAt: number; opacity: number }[]>([]);
  const state = useRef({ selected, active, onSelect, onStep, onUnavailable });
  state.current = { selected, active, onSelect, onStep, onUnavailable };
  const update = useRef<() => void>(() => {});
  useEffect(() => update.current(), [selected, active]);
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
    floor.rotation.x = -Math.PI / 2; floor.position.y = -4; floor.receiveShadow = true; scene.add(floor);
    const group = new THREE.Group(); group.position.y = .12; scene.add(group);
    // Keep a variable file count within the original five-layer envelope.
    const count = files.length + (creation ? 1 : 0);
    const spacing = Math.min(.54, 2.7 / Math.max(1, count - 1));
    const previous = previousFiles.current, previousCount = previous.length + (creation ? 1 : 0), previousSpacing = Math.min(.54, 2.7 / Math.max(1, previousCount - 1));
    const key = (file: PromptFile) => file.layer_id ?? file.name;
    const shift = previous.findIndex(file => key(file) === (files[0] && key(files[0])));
    const direction = shift > 0 ? (previous.length === files.length && shift > files.length / 2 ? -1 : 1) : files.findIndex(file => key(file) === (previous[0] && key(previous[0]))) > 0 ? -1 : 1;
    const cyclic = !!state.current.onStep;
    const wraps = (oldIndex: number, index: number) => cyclic && oldIndex >= 0 && previous.length === files.length && files.length > 1 && (direction > 0 ? oldIndex < Math.max(1, shift) && index > oldIndex : oldIndex === previous.length - 1 && index === 0);
    previousFiles.current = files;
    function createMesh(file: PromptFile, index: number, exiting = false) {
      const color = new THREE.Color();
      const materials = [.92, .95, 1, 1, .9, .93].map(shade => new THREE.MeshStandardMaterial({ color: color.clone().multiplyScalar(shade), roughness: .8 }));
      const width = 4.5, depth = 3.55;
      const thickness = Math.min(.085, Math.min(spacing, previousSpacing) * .16);
      const mesh = new THREE.Mesh(new THREE.BoxGeometry(width, thickness, depth), materials);
      const oldIndex = previous.findIndex(old => key(old) === key(file));
      const wrapped = wraps(oldIndex, index);
      const initial = previous.length === 0;
      const retained = initial || oldIndex >= 0 && (!wrapped || exiting);
      const targetY = ((count - 1) / 2 - index) * spacing;
      const pose = retained ? previousPoses.current.get(key(file)) : undefined;
      const distance = Math.max(spacing, previousSpacing);
      mesh.position.y = initial ? targetY : pose?.y ?? (retained ? ((previousCount - 1) / 2 - oldIndex) * previousSpacing : targetY - direction * distance);
      mesh.position.x = pose?.x ?? 0;
      mesh.castShadow = true; mesh.receiveShadow = true;
      mesh.customDepthMaterial = new THREE.MeshDepthMaterial({ depthPacking: THREE.RGBADepthPacking, alphaHash: true });
      const shadowOpacity = { value: 1 };
      // Packed depth ignores material.opacity; apply the layer fade before shadow coverage.
      mesh.customDepthMaterial.onBeforeCompile = shader => {
        shader.uniforms.layerOpacity = shadowOpacity;
        shader.fragmentShader = "uniform float layerOpacity;\n" + shader.fragmentShader.replace(
          "#include <alphahash_fragment>",
          "diffuseColor.a = layerOpacity;\n#include <alphahash_fragment>",
        );
      };
      const opacity = retained ? pose?.opacity ?? 1 : 0;
      mesh.userData = { index, width, depth, file, exiting, shadowOpacity, startY: mesh.position.y, targetY: exiting ? mesh.position.y + direction * distance : targetY, opacity, initialOpacity: opacity, phaseAt: performance.now(), phase: exiting ? "exit" : retained ? "steady" : "enter" };
      materials.forEach(material => { material.opacity = mesh.userData.opacity; });
      mesh.add(new THREE.LineSegments(new THREE.EdgesGeometry(mesh.geometry), new THREE.LineBasicMaterial({ color: color.clone().multiplyScalar(.78), transparent: true, depthWrite: false, opacity: .32 })));
      group.add(mesh); return mesh;
    }
    const meshes = files.map((file, index) => createMesh(file, index));
    const leaving = previous.filter((file, oldIndex) => { const index = files.findIndex(current => key(current) === key(file)); return index < 0 || wraps(oldIndex, index); }).map((file, index) => createMesh(file, index, true));
    // A second wheel tick must not discard a still-fading layer or its shadow.
    for (const exit of previousExits.current) {
      if (exit.opacity <= 0) continue;
      const mesh = createMesh(exit.file, -1, true);
      Object.assign(mesh.userData, exit, { exiting: true, phase: "exit" });
      mesh.position.set(exit.x, exit.y, 0); leaving.push(mesh);
    }
    const animated = [...meshes, ...leaving];
    const placeholder = creation ? new THREE.Mesh(new THREE.BoxGeometry(4.5, .015, 3.55), new THREE.MeshBasicMaterial({ transparent: true, opacity: .015, depthWrite: false })) : null;
    let outline: THREE.Line<THREE.BufferGeometry, THREE.LineDashedMaterial> | undefined;
    if (placeholder) {
      placeholder.position.y = ((count - 1) / 2 - files.length) * spacing;
      placeholder.userData = { width: 4.5, depth: 3.55 };
      const geometry = new THREE.BufferGeometry().setFromPoints([[-2.25,.012,-1.775],[2.25,.012,-1.775],[2.25,.012,1.775],[-2.25,.012,1.775],[-2.25,.012,-1.775]].map(point => new THREE.Vector3(...point as [number,number,number])));
      outline = new THREE.Line(geometry, new THREE.LineDashedMaterial({ color: "#777777", dashSize: .14, gapSize: .1, transparent: true, opacity: .7 }));
      outline.computeLineDistances(); placeholder.add(outline); group.add(placeholder);
    }
    const raycaster = new THREE.Raycaster(), pointer = new THREE.Vector2(), anchor = new THREE.Vector3();
    const reduced = matchMedia("(prefers-reduced-motion: reduce)");
    let hover = -1, frame = 0, last = 0, visible = true, lost = false, layoutUntil = 0;
    function draw(now: number) {
      frame = 0;
      if (document.hidden || !visible || lost) return;
      const dt = Math.min((now - last) / 1000, .05); last = now;
      const motion = document.documentElement.dataset.reducedMotion;
      const reducedMotion = motion === undefined ? reduced.matches : motion === "true";
      const ease = reducedMotion ? 1 : 1 - Math.exp(-dt * 11);
      let moving = false;
      animated.forEach(mesh => {
        const index = mesh.userData.index, selected = !mesh.userData.exiting && index === state.current.selected;
        const x = selected ? .16 : 0;
        const frame = layerFrame(mesh.userData.startY, mesh.userData.targetY, now - mesh.userData.phaseAt, mesh.userData.phase, mesh.userData.initialOpacity, reducedMotion, spacing);
        const y = frame.y + (!mesh.userData.exiting && index === hover ? .045 : 0);
        moving ||= !frame.done || Math.abs(x - mesh.position.x) > .0001;
        mesh.position.x += (x - mesh.position.x) * ease; mesh.position.y = y;
        mesh.userData.opacity = frame.opacity;
        mesh.visible = frame.opacity > 0; mesh.castShadow = frame.opacity > 0;
        mesh.userData.shadowOpacity.value = frame.opacity;
        mesh.material.forEach(material => {
          const transparent = frame.opacity < .999;
          if (material.transparent !== transparent) { material.transparent = transparent; material.depthWrite = !transparent; material.needsUpdate = true; }
          material.opacity = frame.opacity;
        });
        (mesh.children[0] as THREE.LineSegments<THREE.EdgesGeometry, THREE.LineBasicMaterial>).material.opacity = (selected ? .55 : .25) * mesh.userData.opacity;
      });
      renderer.render(scene, camera);
      const mesh = meshes[state.current.selected];
      const svg = connection.current?.ownerSVGElement ?? creation?.current?.ownerSVGElement;
      if (svg) {
        const sceneBounds = element.getBoundingClientRect(), bounds = svg.getBoundingClientRect();
        const project = (page: THREE.Object3D, x: number, y: number, z: number) => {
          anchor.set(x, y, z); page.localToWorld(anchor); anchor.project(camera);
          return { x: sceneBounds.left - bounds.left + (anchor.x + 1) * sceneBounds.width / 2, y: sceneBounds.top - bounds.top + (-anchor.y + 1) * sceneBounds.height / 2 };
        };
        const outward = (page: THREE.Object3D, x: number, z: number, dx: number, dz: number) => {
          const start = project(page, x, .045, z);
          return { start, lead: projectedSurfaceLead(start, project(page, x + dx, .045, z + dz)) };
        };
        const plates = [...meshes, ...(placeholder ? [placeholder] : [])].map(page => {
          const w = page.userData.width / 2, d = page.userData.depth / 2;
          return [[-w,-d],[w,-d],[w,d],[-w,d]].map(([x,z]) => project(page, x, .045, z));
        });
        const section = element.closest("section")!;
        const route = (path: SVGPathElement, edge: ReturnType<typeof outward>, end: { x: number; y: number }, normal: { x: number; y: number }, button?: HTMLButtonElement) => {
          const controls = [...section.querySelectorAll<HTMLElement>("button,[data-slot=input-group]")].filter(control => control !== button && getComputedStyle(control).visibility !== "hidden").map(control => {
            const rect = control.getBoundingClientRect(), l = rect.left - bounds.left - 4, r = rect.right - bounds.left + 4, t = rect.top - bounds.top - 4, b = rect.bottom - bounds.top + 4;
            return [{ x: l, y: t }, { x: r, y: t }, { x: r, y: b }, { x: l, y: b }];
          });
          path.setAttribute("d", promptConnector(edge.start, edge.lead, end, normal, [...plates, ...controls], bounds));
        };
        if (state.current.active && mesh && connection.current && target.current) {
          const end = target.current.getBoundingClientRect();
          const edge = creation ? outward(mesh, 0, mesh.userData.depth / 2, 0, 1) : outward(mesh, mesh.userData.width / 2, 0, 1, 0);
          route(connection.current, edge, { x: end.left + end.width / 2 - bounds.left, y: end.top + end.height / 2 - bounds.top }, { x: 0, y: -1 });
          if (removal.current && removeTarget.current && files[state.current.selected]?.layer_id) {
            const button = removeTarget.current.getBoundingClientRect();
            const lower = outward(mesh, 0, mesh.userData.depth / 2, 0, 1);
            route(removal.current, lower, { x: button.right - bounds.left, y: button.top + button.height / 2 - bounds.top }, { x: 1, y: 0 }, removeTarget.current);
          }
        }
        if (placeholder && creation?.current && createTarget?.current) {
          const button = createTarget.current.getBoundingClientRect(), edge = outward(placeholder, 2.25, 0, 1, 0);
          route(creation.current, edge, { x: button.left - bounds.left, y: button.top + button.height / 2 - bounds.top }, { x: -1, y: 0 }, createTarget.current);
        }
      }
      if (moving || now < layoutUntil) frame = requestAnimationFrame(draw);
    }
    function invalidate() { if (!frame) { last = performance.now(); frame = requestAnimationFrame(draw); } }
    update.current = invalidate;
    // Resolve CSS theme colors without a second visible file list.
    const pixel = document.createElement("canvas"); pixel.width = pixel.height = 1;
    const paint = pixel.getContext("2d", { willReadFrequently: true });
    const probe = document.createElement("span"); probe.hidden = true; element.append(probe);
    function palette() {
      if (paint) animated.forEach(mesh => {
        probe.style.background = layerColor((mesh.userData.file as PromptFile).tokens, files.length ? files : previous);
        paint.clearRect(0, 0, 1, 1); paint.fillStyle = getComputedStyle(probe).backgroundColor; paint.fillRect(0, 0, 1, 1);
        const [r, g, b] = paint.getImageData(0, 0, 1, 1).data;
        const color = new THREE.Color().setRGB(r / 255, g / 255, b / 255, THREE.SRGBColorSpace);
        mesh.material.forEach((material, side) => material.color.copy(color).multiplyScalar([.92, .95, 1, 1, .9, .93][side]));
        (mesh.children[0] as THREE.LineSegments<THREE.EdgesGeometry, THREE.LineBasicMaterial>).material.color.copy(color).multiplyScalar(.78);
      });
      if (outline && paint) { probe.style.background = "var(--muted-foreground)"; paint.fillStyle = getComputedStyle(probe).backgroundColor; paint.fillRect(0,0,1,1); const [r,g,b] = paint.getImageData(0,0,1,1).data; outline.material.color.setRGB(r/255,g/255,b/255,THREE.SRGBColorSpace); }
      invalidate();
    }
    const theme = new MutationObserver(palette); theme.observe(document.documentElement, { attributes: true, attributeFilter: ["class", "style", "data-reduced-motion"] });
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
    let downX = 0, downY = 0, pointerId: number | undefined;
    const down = (event: PointerEvent) => {
      if (!event.isPrimary || event.button !== 0) { pointerId = undefined; return; }
      pointerId = event.pointerId; downX = event.clientX; downY = event.clientY; surface.setPointerCapture(event.pointerId);
    };
    const click = (event: PointerEvent) => {
      if (event.pointerId !== pointerId) return;
      pointerId = undefined; hover = -1; invalidate();
      const dx = event.clientX - downX, dy = downY - event.clientY;
      if (state.current.onStep && Math.abs(dy) > 24 && Math.abs(dy) > Math.abs(dx) * 1.2) { state.current.onStep(Math.sign(dy)); return; }
      if (Math.hypot(dx, dy) > 8) return;
      const index = hit(event); if (index >= 0) state.current.onSelect(index);
    };
    const cancel = () => { pointerId = undefined; hover = -1; invalidate(); };
    let accumulated = 0, wheelAt = 0;
    const wheel = (event: WheelEvent) => {
      if (!state.current.onStep || event.ctrlKey || Math.abs(event.deltaX) > Math.abs(event.deltaY)) return;
      event.preventDefault(); accumulated += event.deltaY * (event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? element.clientHeight : 1);
      if (Math.abs(accumulated) >= 24 && performance.now() - wheelAt > 150) { state.current.onStep(Math.sign(accumulated)); accumulated = 0; wheelAt = performance.now(); }
    };
    const section = element.closest("section");
    const transition = () => { layoutUntil = performance.now() + 500; invalidate(); };
    section?.addEventListener("transitionrun", transition);
    const contextLost = (event: Event) => { event.preventDefault(); lost = true; state.current.onUnavailable(); };
    surface.addEventListener("pointermove", move); surface.addEventListener("pointerleave", leave); surface.addEventListener("pointerdown", down); surface.addEventListener("pointerup", click);
    surface.addEventListener("pointercancel", cancel); surface.addEventListener("wheel", wheel, { passive: false });
    surface.addEventListener("webglcontextlost", contextLost);
    document.addEventListener("visibilitychange", invalidate);
    reduced.addEventListener("change", invalidate);
    const observer = new ResizeObserver(resize); observer.observe(element);
    if (target.current) observer.observe(target.current.parentElement!);
    const intersection = new IntersectionObserver(([entry]) => { visible = entry.isIntersecting; invalidate(); }); intersection.observe(element);
    resize();
    return () => {
      previousPoses.current = new Map(meshes.map(mesh => [key(mesh.userData.file), { y: mesh.position.y, x: mesh.position.x, opacity: mesh.userData.opacity }]));
      previousExits.current = leaving.filter(mesh => mesh.userData.opacity > 0).map(mesh => ({ file: mesh.userData.file, y: mesh.position.y, x: mesh.position.x, startY: mesh.userData.startY, targetY: mesh.userData.targetY, initialOpacity: mesh.userData.initialOpacity, phaseAt: mesh.userData.phaseAt, opacity: mesh.userData.opacity }));
      update.current = () => {}; cancelAnimationFrame(frame); observer.disconnect(); intersection.disconnect(); theme.disconnect();
      surface.removeEventListener("pointermove", move); surface.removeEventListener("pointerleave", leave); surface.removeEventListener("pointerdown", down); surface.removeEventListener("pointerup", click);
      surface.removeEventListener("pointercancel", cancel); surface.removeEventListener("wheel", wheel); section?.removeEventListener("transitionrun", transition);
      surface.removeEventListener("webglcontextlost", contextLost); document.removeEventListener("visibilitychange", invalidate); reduced.removeEventListener("change", invalidate);
      scene.traverse(object => { if (object instanceof THREE.Mesh || object instanceof THREE.Line) { object.geometry.dispose(); const materials = Array.isArray(object.material) ? object.material : [object.material]; materials.forEach(material => material.dispose()); if (object instanceof THREE.Mesh) object.customDepthMaterial?.dispose(); } });
      light.shadow.map?.dispose(); renderer.dispose();
      probe.remove();
    };
  }, [files, connection, target, removal, removeTarget, creation, createTarget]);
  return <div className="dl-prompt-canvas" data-scrollable={!!onStep} ref={host} role="listbox" tabIndex={0} aria-label={t("context_composition.files")} aria-activedescendant={selected >= 0 ? `${listId}-${selected}` : undefined} onKeyDown={event => {
    if ((event.key === "ArrowDown" && selected === files.length - 1) || (event.key === "ArrowUp" && selected === 0)) { if (onStep) { event.preventDefault(); onStep(event.key === "ArrowDown" ? 1 : -1); return; } }
    const next = event.key === "ArrowDown" || event.key === "ArrowRight" ? Math.min(files.length - 1, selected + 1) : event.key === "ArrowUp" || event.key === "ArrowLeft" ? Math.max(0, selected - 1) : event.key === "Home" ? 0 : event.key === "End" ? files.length - 1 : null;
    if (next !== null) { event.preventDefault(); onSelect(next); }
  }}>
    <canvas ref={canvas} aria-hidden="true" />
    <div className="sr-only">{files.map((file, index) => <span key={file.layer_id ?? file.name} id={`${listId}-${index}`} role="option" aria-selected={selected === index}>{file.name}</span>)}</div>
  </div>;
}
