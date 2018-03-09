// vim: set et ts=2 sw=2:
//

const webpack = require('webpack');
const NVRSettings = require('./NVRSettings');
const baseConfig = require('./base.config.js');

module.exports = (env, args) => {
  const settingsObject = new NVRSettings(env, args);
  const nvrSettings = settingsObject.settings;

  return settingsObject.webpackMerge(baseConfig, {
    stats: {
      warnings: true,
    },
    devtool: 'inline-source-map',
    devServer: {
      contentBase: nvrSettings.app_src_dir,
      historyApiFallback: true,
      inline: true,
      port: 3000,
      hot: true,
      clientLogLevel: 'info',
      proxy: {
        '/api': `http://${nvrSettings.moonfire.server}:${nvrSettings.moonfire.port}`,
      },
    },
    plugins: [
      new webpack.DefinePlugin({
        'process.env.NODE_ENV': JSON.stringify('development'),
      }),
      new webpack.HotModuleReplacementPlugin(),
    ],
  });
};
