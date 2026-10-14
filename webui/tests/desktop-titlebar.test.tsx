import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { DesktopTitlebar } from "../src/components/desktop-titlebar";
import i18n from "../src/lib/i18n";

test("browser entry has no native window chrome", () => {
  expect(renderToStaticMarkup(<DesktopTitlebar platform={null} />)).toBe("");
});
test("Windows controls are accessible and never restore branding or status placeholders", async () => {
  await i18n.changeLanguage("en-US");
  const html = renderToStaticMarkup(<DesktopTitlebar platform="windows" />);
  expect(html).toContain('aria-label="Minimize"');
  expect(html).toContain('aria-label="Maximize"');
  expect(html).toContain('aria-label="Close window"');
  expect(html).toContain("data-window-drag-region");
  expect(html).not.toContain("Daat Locus");
  await i18n.changeLanguage("zh-CN");
  expect(renderToStaticMarkup(<DesktopTitlebar platform="windows" />)).toContain('aria-label="关闭窗口"');
});
test("macOS leaves room for native traffic lights without duplicating their controls", () => {
  const html = renderToStaticMarkup(<DesktopTitlebar platform="macos" />);
  expect(html).toContain("data-window-drag-region");
  expect(html).not.toContain("<button");
});
