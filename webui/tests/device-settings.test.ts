import { expect, test } from "bun:test";
import { validConnection } from "../src/lib/device-settings";

test("connection preferences only save complete credential-free WSS origins", () => {
  for (const value of ["", "wss://signal.example", "wss://signal.example:8443/"]) expect(validConnection(value)).toBe(true);
  for (const value of ["https://signal.example", "wss://user:secret@signal.example", "wss://signal.example/path", "wss://signal.example?secret=x", "wss://"]) expect(validConnection(value)).toBe(false);
});
