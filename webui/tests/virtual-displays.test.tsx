import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { NavigationRail } from "../src/components/app-navigation";
import { TooltipProvider } from "../src/components/ui/tooltip";
import i18n from "../src/lib/i18n";
import { ActorApiError } from "../src/lib/actor-api";
import { observeVirtualDisplays, type DisplayCatalog, type DisplayPreview } from "../src/lib/virtual-displays";

const catalog: DisplayCatalog = { supported: true, state: "ready", displays: [
  { id: "a", actor_id: "worker-a", name: "A", width: 1280, height: 720 },
  { id: "b", actor_id: "worker-b", name: "B", width: 1280, height: 720 },
] };
function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>(done => { resolve = done; });
  return { promise, resolve };
}

test("navigation exposes virtual displays only when the device supports them", () => {
  const render = (supported: boolean) => renderToStaticMarkup(<TooltipProvider><NavigationRail access={{ kind: "owner", manage: true }} settings={false} apps={false} virtualDisplays virtualDisplaysSupported={supported} onContacts={() => {}} onApps={() => {}} onHome={() => {}} onSettings={() => {}} onLogout={() => {}} /></TooltipProvider>);
  const label = `aria-label="${i18n.t("virtual_displays.title")}"`;
  expect(render(false)).not.toContain(label);
  const html = render(true);
  expect(html).toContain(label);
  expect(html.match(/aria-current="page"/g)).toHaveLength(1);
});

test("a slow capture applies backpressure and an aborted selection cannot publish its late image", async () => {
  const controller = new AbortController();
  const frame = deferred<Blob | null>(), started = deferred<void>();
  const requested: string[] = [], updates: DisplayPreview[] = [];
  let lists = 0, pauses = 0;
  const run = observeVirtualDisplays(controller.signal, "b", update => updates.push(update), () => { throw new Error("unexpected revocation"); }, {
    catalog: async () => { lists++; return catalog; },
    frame: async id => { requested.push(id); started.resolve(); return frame.promise; },
    pause: async () => { pauses++; },
  });
  await started.promise;
  expect(lists).toBe(1);
  expect(requested).toEqual(["b"]);
  expect(pauses).toBe(0);
  controller.abort();
  frame.resolve(new Blob(["old frame"]));
  await run;
  expect(updates).toEqual([]);
});

test("disappearing screens clear the previous image without capturing a fallback desktop", async () => {
  const controller = new AbortController();
  const updates: DisplayPreview[] = [], requested: string[] = [];
  let lists = 0;
  await observeVirtualDisplays(controller.signal, "a", update => updates.push(update), () => {}, {
    catalog: async () => ++lists === 1 ? catalog : { ...catalog, displays: [] },
    frame: async id => { requested.push(id); return new Blob(["frame"]); },
    pause: async () => { if (lists === 2) controller.abort(); },
  });
  expect(requested).toEqual(["a"]);
  expect(updates[0].image).toBeInstanceOf(Blob);
  expect(updates[1]).toMatchObject({ display: null, image: null, failed: false });
});

test("revocation clears the image and terminates refresh without retrying", async () => {
  const updates: DisplayPreview[] = [];
  let revoked = 0;
  await observeVirtualDisplays(new AbortController().signal, null, update => updates.push(update), () => { revoked++; }, {
    catalog: async () => catalog,
    frame: async () => { throw new ActorApiError("revoked", 401); },
    pause: async () => { throw new Error("must not retry"); },
  });
  expect(updates).toHaveLength(1);
  expect(updates[0]).toMatchObject({ image: null, failed: true });
  expect(revoked).toBe(1);
});

test.each(["disabled", "connecting", "unavailable", "unsupported"] as const)("%s displays are never captured", async state => {
  const controller = new AbortController();
  const updates: DisplayPreview[] = [];
  await observeVirtualDisplays(controller.signal, "a", update => updates.push(update), () => {}, {
    catalog: async () => ({ ...catalog, state }),
    frame: async () => { throw new Error("must not capture"); },
    pause: async () => { controller.abort(); },
  });
  expect(updates).toHaveLength(1);
  expect(updates[0]).toMatchObject({ display: null, image: null, failed: false });
});
