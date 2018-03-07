// vim: set et ts=2 sw=2:
//

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
      rules: [{
        test: /\.js$/,
        loader: 'babel-loader',
        query: {
          'presets': ['env'],
        },
        exclude: /(node_modules|bower_components)/,
        include: ['./ui-src'],
      }, {
        test: /\.png$/,
        use: ['file-loader'],
      }, {
        // Load css and then in-line in head
        test: /\.css$/,
        loader: 'style-loader!css-loader',
      }],
    },
    plugins: [
      new webpack.DefinePlugin({
        'process.env.NODE_ENV': JSON.stringify(args.mode),
      }),
      new webpack.IgnorePlugin(/\.\/locale$/),
      new HtmlWebpackPlugin({
        title: nvrSettings.app_title,
        filename: 'index.html',
        template: path.join(nvrSettings._paths.app_src_dir, 'assets', 'index.html'),
      }),
      new webpack.NormalModuleReplacementPlugin(
        /node_modules\/moment\/moment\.js$/,
        './min/moment.min.js'),
      new webpack.NormalModuleReplacementPlugin(
        /node_modules\/moment-timezone\/index\.js$/,
        './builds/moment-timezone-with-data-2012-2022.min.js'),
    ],
  };
};
