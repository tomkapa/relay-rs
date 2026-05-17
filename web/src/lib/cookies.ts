export function readCookie(name: string): string | null {
  const target = name + "=";
  const cookies = document.cookie ? document.cookie.split("; ") : [];
  for (const c of cookies) {
    if (c.startsWith(target)) return decodeURIComponent(c.slice(target.length));
  }
  return null;
}
