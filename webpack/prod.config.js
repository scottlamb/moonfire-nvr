// vim: set et ts=2 sw=2:
//

const webpack = require('webpack');
const NVRSettings = require('./NVRSettings');
const baseConfig = require('./base.config.js');

const CleanWebpackPlugin = require('clean-webpack-plugin');

module.exports = (env, args) => {
  const settingsObject = new NVRSettings(env, args);
  const nvrSettings = settingsObject.settings;

  return settingsObject.webpackMerge(baseConfig, {
    optimization: {
      splitChunks: {
        cacheGroups: {
          default: {
            minChunks: 2,
            priority: -20,
          },
          commons: {
            test: /[\\/]node_modules[\\/]/,
            name: 'vendor',
            chunks: 'all',
            priority: -10,
          },
        },
      },
    },
    plugins: [
      new webpack.DefinePlugin({
        'process.env.NODE_ENV': JSON.stringify('production'),
      }),
      new CleanWebpackPlugin([nvrSettings.dist_dir], {
        root: nvrSettings._paths.project_root,
      }),
    ],
  });
};
