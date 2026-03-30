// Custom server that wraps Next.js standalone with WebSocket proxy support.
// Proxies /api/ws/* upgrade requests to the backend, delegates all else to Next.js.
const http = require("http");
const path = require("path");

process.env.NODE_ENV = "production";
process.chdir(__dirname);

const NextServer = require("next/dist/server/next-server").default;
const conf = require("./.next/required-server-files.json").config;

const hostname = process.env.HOSTNAME || "0.0.0.0";
const port = parseInt(process.env.PORT, 10) || 3000;
const backendUrl = process.env.BACKEND_URL || "http://backend:3001";

const nextServer = new NextServer({
  hostname,
  port,
  dir: __dirname,
  dev: false,
  customServer: true,
  conf,
});

nextServer.prepare().then(() => {
  const handler = nextServer.getRequestHandler();

  const server = http.createServer((req, res) => {
    handler(req, res);
  });

  // Proxy WebSocket upgrades for /api/ws/*
  server.on("upgrade", (req, socket, head) => {
    if (!req.url || !req.url.startsWith("/api/ws/")) {
      socket.destroy();
      return;
    }

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

    proxyReq.on("response", (res) => {
      // Backend rejected the upgrade (non-101 response)
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
  });

  server.listen(port, hostname, () => {
    console.log(`Server listening on port ${port}`);
  });
});
