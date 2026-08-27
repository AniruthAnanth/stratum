// Serve out/ the way GitHub Pages does: mounted at /stratum/, so basePath
// URLs resolve. `pnpm serve` then open http://localhost:8123/stratum/
//
// Dependency-free on purpose — this is a preview, not infrastructure.

import { createReadStream, existsSync, statSync } from "node:fs";
import { createServer } from "node:http";
import { extname, join, normalize, resolve } from "node:path";

const OUT = resolve(import.meta.dirname, "..", "out");
const BASE = "/stratum";
const PORT = Number(process.env.PORT ?? 8123);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json",
  ".txt": "text/plain; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
};

createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://localhost");
  let path = decodeURIComponent(url.pathname);
  if (path === "/" || path === BASE) {
    res.writeHead(302, { Location: `${BASE}/` });
    return res.end();
  }
  if (!path.startsWith(`${BASE}/`)) {
    res.writeHead(404);
    return res.end("not under /stratum/");
  }
  path = path.slice(BASE.length);
  let file = normalize(join(OUT, path));
  if (!file.startsWith(OUT)) {
    res.writeHead(403);
    return res.end();
  }
  if (existsSync(file) && statSync(file).isDirectory()) file = join(file, "index.html");
  if (!existsSync(file)) {
    const notFound = join(OUT, "404.html");
    res.writeHead(404, { "Content-Type": TYPES[".html"] });
    return existsSync(notFound) ? createReadStream(notFound).pipe(res) : res.end("404");
  }
  res.writeHead(200, {
    "Content-Type": TYPES[extname(file)] ?? "application/octet-stream",
    "Cache-Control": "no-store",
  });
  createReadStream(file).pipe(res);
}).listen(PORT, () => {
  console.log(`serving ${OUT} at http://localhost:${PORT}${BASE}/`);
});
