import { t } from "@/lib/i18n";
/** A browser folder is uploaded as one bounded archive, never as a server path. */
export async function zipFolder(files: File[]): Promise<File> {
  const limit = 32 * 1024 * 1024;
  if (!files.length || files.length > 2000) throw new Error(t("folder_attachment.a_folder_can_contain_up_to_2_000_files"));
  if (files.reduce((total, file) => total + file.size, 0) > limit) throw new Error(t("folder_attachment.folder_size_cannot_exceed_32_mb"));
  const { Zip, ZipDeflate } = await import("fflate");
  const names = new Set<string>();
  const chunks: Uint8Array<ArrayBuffer>[] = [];
  let archiveBytes = 0;
  const archive = new Zip((error, bytes) => {
    if (error) throw error;
    archiveBytes += bytes.byteLength;
    if (archiveBytes > limit) throw new Error(t("folder_attachment.packed_attachment_exceeds_32_mb"));
    chunks.push(new Uint8Array(bytes));
  });
  let totalBytes = 0;
  for (const file of files) {
    const relative = file.webkitRelativePath || file.name;
    if (!relative || /[\\:\u0000-\u001f\u007f]/.test(relative) || relative.split("/").some(part => part === ".." || part === "." || !part)) throw new Error(t("folder_attachment.the_folder_contains_an_invalid_path"));
    if (names.has(relative)) throw new Error(t("folder_attachment.the_folder_contains_duplicate_file_names"));
    names.add(relative);
    const bytes = new Uint8Array(await file.arrayBuffer());
    totalBytes += bytes.byteLength;
    if (totalBytes > limit) throw new Error(t("folder_attachment.folder_size_cannot_exceed_32_mb"));
    const entry = new ZipDeflate(relative, { level: 3 });
    archive.add(entry);
    entry.push(bytes, true);
  }
  archive.end();
  const name = files[0].webkitRelativePath?.split("/")[0] || t("composer.folder");
  return new File(chunks, `${name}.zip`, { type: "application/zip" });
}
