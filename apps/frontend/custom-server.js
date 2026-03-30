// Custom server that wraps Next.js standalone with WebSocket proxy support.
// Uses getRequestHandlers to initialize the full router-server pipeline
// (including _next/static serving), then proxies /api/ws/* upgrades to the backend.
const http = require("http");

process.env.NODE_ENV = "production";
process.chdir(__dirname);

const hostname = process.env.HOSTNAME || "0.0.0.0";
const port = parseInt(process.env.PORT, 10) || 3000;
const backendUrl = process.env.BACKEND_URL || "http://backend:3001";

const nextConfig = require("./.next/required-server-files.json").config;
process.env.__NEXT_PRIVATE_STANDALONE_CONFIG = JSON.stringify(nextConfig);

require("next");
const { getRequestHandlers } = require("next/dist/server/lib/start-server");

async function main() {
  let requestHandler;
  let upgradeHandler;

  const server = http.createServer(async (req, res) => {
    try {
      await requestHandler(req, res);
    } catch (err) {
      console.error(err);
      res.statusCode = 500;
      res.end("Internal Server Error");
    }
  });

  // WebSocket upgrade: proxy /api/ws/* to backend, else delegate to Next.js
  server.on("upgrade", (req, socket, head) => {
    if (req.url && req.url.startsWith("/api/ws/")) {
      const target = new URL(backendUrl);
      const proxyReq = http.request({
        hostname: target.hostname,
        port: target.port || 80,
        path: req.url,
        headers: { ...req.headers, host: target.host },
        method: req.method,
      });

      proxyReq.on("upgrade", (proxyRes, proxySocket, proxyHead) => {
        const lines = ["HTTP/1.1 101 Switching Protocols"];
        for (const [key, value] of Object.entries(proxyRes.headers)) {
          lines.push(`${key}: ${value}`);
        }
        socket.write(lines.join("\r\n") + "\r\n\r\n");
        if (proxyHead && proxyHead.length) {
          socket.write(proxyHead);
        }
        proxySocket.pipe(socket);
        socket.pipe(proxySocket);
        proxySocket.on("error", () => socket.destroy());
        socket.on("error", () => proxySocket.destroy());
      });

      proxyReq.on("response", () => {
        socket.destroy();
      });

      proxyReq.on("error", (err) => {
        console.error("WebSocket proxy error:", err.message);
        socket.destroy();
      });

      if (head && head.length) {
        proxyReq.write(head);
      }
      proxyReq.end();
    } else if (upgradeHandler) {
      upgradeHandler(req, socket, head);
    } else {
      socket.destroy();
    }
  });

  const initResult = await getRequestHandlers({
    dir: __dirname,
    port,
    isDev: false,
    server,
    hostname,
  });

  requestHandler = initResult.requestHandler;
  upgradeHandler = initResult.upgradeHandler;

  server.listen(port, hostname, () => {
    console.log(`Server listening on http://${hostname}:${port}`);
  });
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
