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

const path = require('path');
const webpack = require('webpack');
const HtmlWebpackPlugin = require('html-webpack-plugin');

module.exports = {
  entry: {
    nvr: './ui-src/index.js',
  },
  output: {
    filename: '[name].bundle.js',
    path: path.resolve('./ui-dist/'),
    publicPath: '/',
  },
  module: {
    rules: [
      {
        test: /\.js$/,
        loader: 'babel-loader',
        query: {
          presets: [
            ['@babel/preset-env', {
              targets: {
                esmodules: true,
              },
              modules: false
            }]
          ],
        },
        exclude: /(node_modules|bower_components)/,
        include: [path.resolve('./ui-src')],
      },
      {
        test: /\.png$/,
        use: ['file-loader'],
      },
      {
        test: /\.ico$/,
        use: [
          {
            loader: 'file-loader',
            options: {
              name: '[name].[ext]',
            },
          },
        ],
      },
      {
        // Load css and then in-line in head
        test: /\.css$/,
        use: ['style-loader', 'css-loader'],
      },
    ],
  },
  plugins: [
    new webpack.IgnorePlugin(/\.\/locale$/),
    new HtmlWebpackPlugin({
      title: 'Moonfire NVR',
      filename: 'index.html',
      template: './ui-src/assets/index.html',
    }),
    new webpack.NormalModuleReplacementPlugin(
      /node_modules\/moment\/moment\.js$/,
      './min/moment.min.js'
    ),
    new webpack.NormalModuleReplacementPlugin(
      /node_modules\/moment-timezone\/index\.js$/,
      './builds/moment-timezone-with-data-2012-2022.min.js'
    ),
  ],
};
