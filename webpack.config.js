const path = require('path');
const webpack = require('webpack');
const MinifyPlugin = require("babel-minify-webpack-plugin");

module.exports = {
  entry: './ui-src/index.js',
  output: {
    filename: 'bundle.js',
    path: path.resolve(__dirname, 'ui-dist')
  },
  module: {
    loaders: [
      { test: /\.png$/, loader: "file-loader" },
      { test: /\.css$/, loader: "style-loader!css-loader" },
    ]
  },
  plugins: [
    new webpack.NormalModuleReplacementPlugin(
        /node_modules\/moment\/moment\.js$/,
        './min/moment.min.js'),
    new webpack.IgnorePlugin(/\.\/locale$/),
    new webpack.NormalModuleReplacementPlugin(
        /node_modules\/moment-timezone\/index\.js$/,
        './builds/moment-timezone-with-data-2012-2022.min.js'),
    new MinifyPlugin({}, {})
  ]
};
