import { test, expect } from "bun:test";
import { wasmBuildEnvironment } from "../scripts/build-auth";
test("nested WASM build drops host/Android flags but retains explicit WASM settings", () => {
  const source = { RUSTFLAGS: "-l log", CARGO_ENCODED_RUSTFLAGS: "-l\x1fandroid", CARGO_BUILD_TARGET: "aarch64-linux-android", CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS: "-C target-feature=+bulk-memory", PATH: "toolchain" };
  expect(wasmBuildEnvironment(source)).toEqual({ CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS: source.CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUSTFLAGS, PATH: "toolchain" });
  expect(source.RUSTFLAGS).toBe("-l log");
});
