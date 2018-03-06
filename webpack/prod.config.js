const path = require('path');
const webpack = require('webpack');
const merge = require('webpack-merge');
const baseConfig = require('./base.config.js');

const CleanWebpackPlugin = require('clean-webpack-plugin');

module.exports = merge(baseConfig, {
  plugins: [
    new webpack.DefinePlugin({
      'process.env.NODE_ENV': JSON.stringify('production'),
    }),
    new CleanWebpackPlugin(['ui-dist'], { root: path.resolve(__dirname, '../') }),
  ],
});
