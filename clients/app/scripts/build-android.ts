import { existsSync } from "node:fs";
import { resolve } from "node:path";
const args = process.argv.slice(2);
const debug = args.includes("--debug");
const target = args.includes("--target") ? args[args.indexOf("--target") + 1] : "aarch64";
if (!target || !["aarch64", "x86_64"].includes(target)) throw new Error("Unsupported Android target");
if (!debug && (!process.env.DAAT_LOCUS_ANDROID_SIGNING || !existsSync(process.env.DAAT_LOCUS_ANDROID_SIGNING))) {
  throw new Error("DAAT_LOCUS_ANDROID_SIGNING must reference private release signing properties");
}
const child = Bun.spawn([process.execPath, "run", "tauri", "android", "build", "--features", "custom-protocol", "--target", target, "--apk", ...(debug ? ["--debug"] : [])], {
  cwd: resolve(import.meta.dir, ".."), env: { ...process.env, DAAT_LOCUS_BUILD_WEBUI: "0", DAAT_LOCUS_ANDROID_ABI: target === "aarch64" ? "arm64-v8a" : "x86_64" }, stdin: "inherit", stdout: "inherit", stderr: "inherit",
});
process.exit(await child.exited);
