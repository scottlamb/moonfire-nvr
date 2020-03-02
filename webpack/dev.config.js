// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

const merge = require('webpack-merge');
const webpack = require('webpack');
const baseConfig = require('./base.config.js');

module.exports = merge(baseConfig, {
  stats: {
    warnings: true,
  },
  devtool: 'inline-source-map',
  mode: 'development',
  optimization: {
    minimize: false,
    namedChunks: true,
  },
  devServer: {
    inline: true,
    port: process.env.MOONFIRE_DEV_PORT || 3000,
    host: process.env.MOONFIRE_DEV_HOST,
    hot: true,
    clientLogLevel: 'info',
    proxy: {
      '/api': {
        target: process.env.MOONFIRE_URL || 'http://localhost:8080/',


        // The live stream URLs require WebSockets.
        ws: true,

        // Change the Host: header so the name-based virtual hosts work
        // properly.
        changeOrigin: true,

        // If the backing host is https, Moonfire NVR will set a 'secure'
        // attribute on cookie responses, so that the browser will only send
        // them over https connections. This is a good security practice, but
        // it means a non-https development proxy server won't work. Strip out
        // this attribute in the proxy with code from here:
        // https://github.com/chimurai/http-proxy-middleware/issues/169#issuecomment-575027907
        // See also discussion in guide/developing-ui.md.
        onProxyRes: (proxyRes, req, res) => {
          const sc = proxyRes.headers['set-cookie'];
          if (Array.isArray(sc)) {
            proxyRes.headers['set-cookie'] = sc.map(sc => {
              return sc.split(';')
                .filter(v => v.trim().toLowerCase() !== 'secure')
                .join('; ')
            });
          }
        },
      },
    },
  },
  plugins: [new webpack.HotModuleReplacementPlugin()],
});
