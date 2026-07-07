// Dev aid: serve the launcher frontend (src/) as a static site so you can
// tweak the UI in a browser without rebuilding the whole Tauri app. Outside
// Tauri, src/main.js falls back to an in-memory mock, so the UI is fully
// clickable here. This is NOT how the real app runs — for that use `cargo run`.
//
//   node tools/preview-ui.js src 5599   ->   http://localhost:5599
const http = require("http");
const fs = require("fs");
const path = require("path");
const root = process.argv[2] || "src";
const port = Number(process.argv[3] || 5599);
const types = {
  ".html": "text/html",
  ".css": "text/css",
  ".js": "text/javascript",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".ico": "image/x-icon",
  ".json": "application/json",
};
http
  .createServer((req, res) => {
    let p = decodeURIComponent(req.url.split("?")[0]);
    if (p === "/") p = "/index.html";
    const fp = path.join(root, p);
    fs.readFile(fp, (e, data) => {
      if (e) {
        res.writeHead(404);
        res.end("not found");
        return;
      }
      res.writeHead(200, { "Content-Type": types[path.extname(fp)] || "application/octet-stream" });
      res.end(data);
    });
  })
  .listen(port, () => console.log("serving " + root + " on http://localhost:" + port));
