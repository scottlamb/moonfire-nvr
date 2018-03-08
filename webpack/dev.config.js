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
    port: 3000,
    hot: true,
    clientLogLevel: 'info',
    proxy: {
	    '/api': 'http://localhost:8080'
    }
  },
  plugins: [
    new webpack.DefinePlugin({
      'process.env.NODE_ENV': JSON.stringify('development'),
    }),
    new webpack.HotModuleReplacementPlugin(),
  ],
});
