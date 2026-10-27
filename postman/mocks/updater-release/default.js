/** 本地更新模拟：按 Tauri 静态清单契约返回签名假安装包，不可用于真实安装。 */
const http = require("node:http");
const signature = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVSK2NVNFZvSWdMZU8yMGhGOWhMaTEwOVBjVnhkcE5JZHhNK3RUaTNWMiswcGtzekNFRTJQMittaUx2b1dvQUQrbldtM0tzbWlHNjBEcVF0Y0krdHRwTVphK2tmRkRRbWc4PQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzkzMDA3MTA4CWZpbGU6cGFja2FnZS5iaW4JdmVyc2lvbjo5LjkuOQozK3ZydXowUng3dHRrY29yMWgzUTk1ZW5iT3VlT2NnLzVMWjdLS2gvc09aSXZweUlwcDJ0N1RrYkExUnlVYVNLSUFsSlRnN2J1MlIrc0ZZQVlHTmFEUT09Cg==";
http.createServer((req, res) => {
  const origin = `http://${req.headers.host}/github`;
  const path = req.url.split("?")[0];
  if (path === "/unavailable.json") { res.writeHead(503); return res.end("upstream unavailable"); }
  if (path === "/missing.json") { res.writeHead(404); return res.end("not found"); }
  if (path === "/invalid.json") { res.writeHead(200, {"Content-Type":"text/html"}); return res.end("<html>proxy error</html>"); }
  if (["/package.bin", "/corrupt.bin"].includes(path)) {
    const bytes = Buffer.from(path === "/package.bin" ? "mpider-updater-test-package" : "tampered");
    res.writeHead(200, {"Content-Type":"application/octet-stream", "Content-Length":bytes.length});
    return res.end(bytes);
  }
  if (["/latest.json", "/corrupt.json"].includes(path)) {
    res.writeHead(200, {"Content-Type":"application/json"});
    return res.end(JSON.stringify({version:"9.9.9", notes:"模拟更新说明", platforms:Object.fromEntries(["test-target", "windows-x86_64"].map(target => [target, {
      url:`${origin}/${path === "/corrupt.json" ? "corrupt.bin" : "package.bin"}`, signature
    }]))}));
  }
  res.writeHead(501); res.end("此模拟未定义的接口");
}).listen(process.env.PORT);
