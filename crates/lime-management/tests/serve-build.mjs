// Isolated production-bundle QA with the same CSP as the Tauri window.
import { createServer } from "node:http";
import { randomBytes } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { extname, resolve, sep } from "node:path";
const root = fileURLToPath(new URL("../dist/", import.meta.url));
const fixture = fileURLToPath(new URL("./tauri-fixture.js", import.meta.url));
const config = JSON.parse(await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"));
const mime = { ".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css; charset=utf-8", ".svg": "image/svg+xml" };
createServer(async (request, response) => {
  try {
    const pathname = decodeURIComponent(new URL(request.url, "http://127.0.0.1").pathname);
    const path = pathname === "/qa-fixture.js" ? fixture : resolve(root, "." + (pathname === "/" ? "/index.html" : pathname));
    if (path !== fixture && !path.startsWith(root.endsWith(sep) ? root : root + sep)) { response.writeHead(403).end(); return; }
    let content = await readFile(path);
    let csp = config.app.security.csp;
    if (extname(path) === ".html") {
      const nonce = randomBytes(16).toString("base64");
      content = Buffer.from(content.toString().replace("<head>", '<head><script src="/qa-fixture.js"></script>').replace("<title>Lime</title>", "<title>Lime · 生产构建验证（模拟数据）</title>").replaceAll("__TAURI_STYLE_NONCE__", nonce));
      csp = csp.replace("style-src 'self'", `style-src 'self' 'nonce-${nonce}'`);
    }
    response.writeHead(200, { "Content-Type": mime[extname(path)] ?? "application/octet-stream", "Content-Security-Policy": csp, "Cache-Control": "no-store" });
    response.end(content);
  } catch { response.writeHead(404).end(); }
}).listen(1421, "127.0.0.1", () => process.stdout.write("Production QA: http://127.0.0.1:1421\n"));
