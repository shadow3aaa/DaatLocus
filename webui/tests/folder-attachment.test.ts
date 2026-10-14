import i18n from "../src/lib/i18n";
import { beforeEach, describe, expect, test } from "bun:test";
import { Unzip, UnzipInflate, unzipSync } from "fflate";
import { zipFolder } from "../src/lib/folder-attachment";

const limit = 32 * 1024 * 1024;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

function file(path: string, contents: string | Uint8Array = "") {
  const value = new File([typeof contents === "string" ? encoder.encode(contents) : new Uint8Array(contents)], path.split("/").at(-1) || "file");
  Object.defineProperty(value, "webkitRelativePath", { value: path });
  return value;
}

async function unpack(archive: File) {
  return unzipSync(new Uint8Array(await archive.arrayBuffer()));
}

describe("folder attachment archives", () => {
  test("real ZIP extraction preserves nested Unicode paths and original bytes", async () => {
    const binary = new Uint8Array([0, 255, 128, 10]);
    const archive = await zipFolder([
      file("项目/说明.md", "你好，DaatLocus。"),
      file("项目/assets/data.bin", binary),
      file("项目/empty.txt"),
    ]);
    expect(archive.name).toBe("项目.zip");
    expect(archive.type).toBe("application/zip");
    const entries = await unpack(archive);
    expect(Object.keys(entries).sort()).toEqual(["项目/assets/data.bin", "项目/empty.txt", "项目/说明.md"].sort());
    expect(decoder.decode(entries["项目/说明.md"])).toBe("你好，DaatLocus。");
    expect(entries["项目/assets/data.bin"]).toEqual(binary);
    expect(entries["项目/empty.txt"].length).toBe(0);
  });

  test("ordinary File inputs and object prototype names remain valid filenames", async () => {
    const archive = await zipFolder([new File(["plain"], "plain.txt"), new File(["proto"], "__proto__"), new File(["ctor"], "constructor")]);
    expect(archive.name).toBe("文件夹.zip");
    // A Map avoids the inherited-key behavior of object-based ZIP decoders too.
    const entries = new Map<string, Uint8Array>();
    const unzip = new Unzip(entry => {
      const chunks: Uint8Array[] = [];
      entry.ondata = (error, bytes, final) => {
        if (error) throw error;
        chunks.push(bytes);
        if (final) {
          const result = new Uint8Array(chunks.reduce((size, chunk) => size + chunk.length, 0));
          let offset = 0;
          for (const chunk of chunks) { result.set(chunk, offset); offset += chunk.length; }
          entries.set(entry.name, result);
        }
      };
      entry.start();
    });
    unzip.register(UnzipInflate);
    unzip.push(new Uint8Array(await archive.arrayBuffer()), true);
    expect([...entries.keys()]).toEqual(["plain.txt", "__proto__", "constructor"]);
    expect(decoder.decode(entries.get("plain.txt"))).toBe("plain");
    expect(decoder.decode(entries.get("__proto__"))).toBe("proto");
    expect(decoder.decode(entries.get("constructor"))).toBe("ctor");
  });

  test("path traversal, absolute paths, drive names, separators and control bytes are rejected", async () => {
    for (const path of ["../secret", "folder/../secret", "folder/./file", "/absolute", "folder//file", "folder/", "C:/secret", "folder\\file", "folder/a\0b", "folder/a\nb", "folder/a\u007fb"]) {
      await expect(zipFolder([file(path, "secret")])).rejects.toThrow("路径");
    }
    await expect(zipFolder([new File(["secret"], "../secret")])).rejects.toThrow("路径");
  });

  test("duplicate paths are rejected even when both files are empty", async () => {
    await expect(zipFolder([file("folder/same"), file("folder/same")])).rejects.toThrow("重名");
    const entries = await unpack(await zipFolder([file("folder/a/same", "one"), file("folder/b/same", "two")]));
    expect(decoder.decode(entries["folder/a/same"])).toBe("one");
    expect(decoder.decode(entries["folder/b/same"])).toBe("two");
  });

  test("zero or over 2000 entries fail while exactly 2000 extract successfully", async () => {
    await expect(zipFolder([])).rejects.toThrow("2000");
    const files = Array.from({ length: 2000 }, (_, i) => file(`folder/${i}.txt`));
    const entries = await unpack(await zipFolder(files));
    expect(Object.keys(entries).length).toBe(2000);
    await expect(zipFolder([...files, file("folder/2000.txt")])).rejects.toThrow("2000");
  });

  test("the uncompressed size boundary permits 32 MiB and rejects a single extra byte", async () => {
    const contents = new Uint8Array(limit);
    const archive = await zipFolder([file("folder/boundary.bin", contents)]);
    expect(archive.size).toBeLessThan(limit);
    expect((await unpack(archive))["folder/boundary.bin"].length).toBe(limit);
    await expect(zipFolder([file("folder/boundary.bin", contents), file("folder/extra", "x")])).rejects.toThrow("文件夹大小");
  });

  test("ZIP overhead and incompressible content cannot exceed the attachment size bound", async () => {
    const bytes = new Uint8Array(limit);
    let state = 0x12345678;
    for (let i = 0; i < bytes.length; i++) {
      state ^= state << 13;
      state ^= state >>> 17;
      state ^= state << 5;
      bytes[i] = state & 255;
    }
    await expect(zipFolder([file("folder/random.bin", bytes)])).rejects.toThrow("打包后的附件");
  });
});

beforeEach(async () => { await i18n.changeLanguage("zh-CN"); });
