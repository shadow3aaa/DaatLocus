import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";

const root = resolve(import.meta.dir, "..");
const rows = readFileSync(resolve(root, "assets/wordmark.txt"), "utf8").trimEnd().split(/\r?\n/);
if (!rows.length || rows.some(row => row.length !== rows[0].length || /[^.CVS]/.test(row))) throw new Error("Invalid wordmark pixel grid");
const paths = { C: "", V: "", S: "" };
for (const [y, row] of rows.entries()) for (const [x, pixel] of [...row].entries()) {
  if (pixel === "C" || pixel === "V" || pixel === "S") paths[pixel] += `M${x * 10} ${y * 10}h10v10h-10z`;
}
const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ${rows[0].length * 10 + 40} ${rows.length * 10 + 40}" role="img" aria-labelledby="title">
<title id="title">DaatLocus</title>
<style>.daat{fill:#656363}.locus{fill:#211e1e}.shade{fill:#cfcecd}@media(prefers-color-scheme:dark){.daat{fill:#b7b1b1}.locus{fill:#f1ecec}.shade{fill:#4b4646}}</style>
<g transform="translate(20 20)" shape-rendering="crispEdges"><path class="shade" d="${paths.S}"/><path class="daat" d="${paths.C}"/><path class="locus" d="${paths.V}"/></g>
</svg>
`;
const output = resolve(root, "assets/wordmark.svg");
if (process.argv.includes("--check")) {
  if (readFileSync(output, "utf8") !== svg) throw new Error("Run bun scripts/render-wordmark.ts to refresh the README wordmark");
} else writeFileSync(output, svg);
