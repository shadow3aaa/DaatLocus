import { expect, test } from "bun:test";
import { searchPreview } from "../src/lib/search-preview";

test("search previews keep useful text without Markdown scaffolding", () => {
  const source = "# Result\n**Ready** [report](https://example.com/report)\n\n| File | Value |\n| --- | --- |\n| audit_file.ts | `16px` |\n\n```css\npadding: 16px;\n```";
  const text = searchPreview(source);
  expect(text).toContain("Ready report");
  expect(text).toContain("audit_file.ts");
  expect(text).toContain("padding: 16px;");
  expect(text).not.toContain("https://");
  expect(text).not.toContain("```");
  expect(text).not.toContain("---");
  expect(text).not.toContain("**");
  for (const value of ["File", "Value", "audit_file.ts", "16px"]) expect(text).toContain(value);
});

test("plain identifiers and Chinese search terms remain searchable", () => {
  expect(searchPreview("检查 audit_file_name.ts 的间距：16px。")).toBe("检查 audit_file_name.ts 的间距：16px。");
});
