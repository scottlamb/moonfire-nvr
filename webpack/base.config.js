// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2018 Dolf Starreveld <dolf@starreveld.com>
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
const NVRSettings = require('./NVRSettings');

module.exports = (env, args) => {
  const nvrSettings = new NVRSettings(env, args).settings;

  return {
    entry: {
      nvr: path.join(nvrSettings._paths.app_src_dir, 'index.js'),
    },
    output: {
      filename: '[name].bundle.js',
      path: nvrSettings._paths.dist_dir,
      publicPath: '/',
    },
    module: {
      rules: [
        {
          test: /\.js$/,
          loader: 'babel-loader',
          query: {
            presets: ['env', {modules: false}],
          },
          exclude: /(node_modules|bower_components)/,
          include: ['./ui-src'],
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
        title: nvrSettings.app_title,
        filename: 'index.html',
        template: path.join(
          nvrSettings._paths.app_src_dir,
          'assets',
          'index.html'
        ),
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
};
