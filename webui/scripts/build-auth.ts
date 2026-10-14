import { spawnSync } from "node:child_process";
import { resolve } from "node:path";

export function wasmBuildEnvironment(source: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const env = { ...source };
  // This is a separate WASM build; Tauri's Android linker flags must not leak in.
  delete env.RUSTFLAGS;
  delete env.CARGO_ENCODED_RUSTFLAGS;
  delete env.CARGO_BUILD_TARGET;
  return env;
}
if (import.meta.main && process.env.DAAT_LOCUS_AUTH_WASM_READY !== "1") {
  const workspace = resolve(import.meta.dir, "../..");
  const result = spawnSync("wasm-pack", ["build", "crates/daat-locus-auth", "--target", "web", "--out-dir", "../../webui/src/generated/account-pake", "--", "--features", "browser", "--locked"], {
    cwd: workspace, stdio: "inherit", env: { ...wasmBuildEnvironment(process.env), CARGO_TARGET_DIR: resolve(workspace, "target/account-wasm") },
  });
  if (result.error) throw result.error;
  if (result.status !== 0) process.exit(result.status ?? 1);
}
