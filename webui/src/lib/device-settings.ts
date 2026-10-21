export function validConnection(value: string): boolean {
  if (!value) return true;
  try {
    const url = new URL(value);
    return url.protocol === "wss:" && url.pathname === "/" && !url.search && !url.hash && !url.username && !url.password;
  } catch { return false; }
}
