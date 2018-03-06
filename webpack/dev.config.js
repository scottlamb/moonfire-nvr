const path = require('path');
const webpack = require('webpack');
const merge = require('webpack-merge');

const baseConfig = require('./base.config.js');

module.exports = merge(baseConfig, {
  devtool: 'inline-source-map',
  devServer: {
    contentBase: './ui-src',
    historyApiFallback: true,
    inline: true,
    host: '0.0.0.0',
    port: 3000,
    hot: true,
    proxy: {
      '/api': 'http://192.168.10.232:8080',
    },
    clientLogLevel: 'info',
  },
  plugins: [
    new webpack.DefinePlugin({
      'process.env.NODE_ENV': JSON.stringify('production'),
    }),
    new webpack.HotModuleReplacementPlugin(),
  ],
});
