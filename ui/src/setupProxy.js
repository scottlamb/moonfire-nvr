// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

// https://create-react-app.dev/docs/proxying-api-requests-in-development/

const { createProxyMiddleware } = require("http-proxy-middleware");

module.exports = (app) => {
  app.use(
    "/api",
    createProxyMiddleware({
      target: process.env.PROXY_TARGET ?? "http://localhost:8080/",
      ws: true,
      changeOrigin: true,

      // If the backing host is https, Moonfire NVR will set a 'secure'
      // attribute on cookie responses, so that the browser will only send
      // them over https connections. This is a good security practice, but
      // it means a non-https development proxy server won't work. Strip out
      // this attribute in the proxy with code from here:
      // https://github.com/chimurai/http-proxy-middleware/issues/169#issuecomment-575027907
      // See also discussion in guide/developing-ui.md.
      onProxyRes: (proxyRes, req, res) => {
        const sc = proxyRes.headers["set-cookie"];
        if (Array.isArray(sc)) {
          proxyRes.headers["set-cookie"] = sc.map((sc) => {
            return sc
              .split(";")
              .filter((v) => v.trim().toLowerCase() !== "secure")
              .join("; ");
          });
        }
      },
    })
  );
};
