// vim: set et ts=2 sw=2:
//

const webpack = require('webpack');
const CompressionPlugin = require('compression-webpack-plugin');
const NVRSettings = require('./NVRSettings');
const baseConfig = require('./base.config.js');

const CleanWebpackPlugin = require('clean-webpack-plugin');

module.exports = (env, args) => {
  const settingsObject = new NVRSettings(env, args);
  const nvrSettings = settingsObject.settings;

  return settingsObject.webpackMerge(baseConfig, {
    //devtool: 'cheap-module-source-map',
    module: {
      rules: [{
        test: /\.html$/,
        loader: 'html-loader',
        query: {
          minimize: true,
        },
      }],
    },
    optimization: {
      minimize: true,
      splitChunks: {
        minSize: 30000,
        minChunks: 1,
        maxAsyncRequests: 5,
        maxInitialRequests: 3,
        cacheGroups: {
          default: {
            minChunks: 2,
            priority: -20,
          },
          commons: {
            name: 'commons',
            chunks: 'all',
            minChunks: 2,
          },
          vendors: {
            test: /[\\/]node_modules[\\/]/,
            name: 'vendor',
            chunks: 'all',
            priority: -10,
          },
        },
      },
    },
    plugins: [
      new CleanWebpackPlugin([nvrSettings.dist_dir], {
        root: nvrSettings._paths.project_root,
      }),
      new CompressionPlugin({
        asset: '[path].gz[query]',
        algorithm: 'gzip',
        test: /\.js$|\.css$|\.html$/,
        threshold: 10240,
        minRatio: 0.8,
      }),
      new webpack.NormalModuleReplacementPlugin(
        /node_modules\/jquery\/dist\/jquery\.js$/,
        './jquery.min.js'),
    ],
  });
};
