// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2023 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import viteCompression from "vite-plugin-compression";

const target = process.env.PROXY_TARGET ?? "http://localhost:8080/";

// https://vitejs.dev/config/
export default defineConfig({
  plugins: [react(), viteCompression()],
  server: {
    proxy: {
      "/api": {
        target,

        // Moonfire NVR needs WebSocket connections for live connections (and
        // likely more in the future:
        // <https://github.com/scottlamb/moonfire-nvr/issues/40>.)
        ws: true,
        changeOrigin: true,

        // If the backing host is https, Moonfire NVR will set a `secure`
        // attribute on cookie responses, so that the browser will only send
        // them over https connections. This is a good security practice, but
        // it means a non-https development proxy server won't work. Strip out
        // this attribute in the proxy with code from here:
        // https://github.com/chimurai/http-proxy-middleware/issues/169#issuecomment-575027907
        // See also discussion in guide/developing-ui.md.
        configure: (proxy, options) => {
          // The `changeOrigin` above doesn't appear to apply to websocket
          // requests. This has a similar effect.
          proxy.on("proxyReqWs", (proxyReq, req, socket, options, head) => {
            proxyReq.setHeader("origin", target);
          });

          proxy.on("proxyRes", (proxyRes, req, res) => {
            const sc = proxyRes.headers["set-cookie"];
            if (Array.isArray(sc)) {
              proxyRes.headers["set-cookie"] = sc.map((sc) => {
                return sc
                  .split(";")
                  .filter((v) => v.trim().toLowerCase() !== "secure")
                  .join("; ");
              });
            }
          });
        },
      },
    },
  },
});
