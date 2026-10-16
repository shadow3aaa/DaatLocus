import { resolve } from "node:path";

const cwd = resolve(import.meta.dir, "../webui");
for (const args of [["install", "--frozen-lockfile"], ["run", "build"]]) {
  const child = Bun.spawn([process.execPath, ...args], {
    cwd, stdin: "inherit", stdout: "inherit", stderr: "inherit",
    env: { ...process.env, DAAT_LOCUS_AUTH_WASM_READY: "0" },
  });
  const code = await child.exited;
  if (code !== 0) process.exit(code);
}
