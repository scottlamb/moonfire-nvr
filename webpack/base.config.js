const path = require('path');
const webpack = require('webpack');


const project_root = path.join(__dirname, '../');
const src_dir = path.join(project_root, 'ui-src');
const dist_dir = path.join(project_root, 'ui-dist');

module.exports = {
  entry: {
    nvr: path.join(src_dir, 'index.js'),
  },
  output: {
    filename: 'bundle.js',
    path: path.resolve(dist_dir),
  },
  module: {
    rules: [{
      test: /\.js$/,
      loader: 'babel',
      query: {
        'presets': ['env', {}],
      },
      include: [path.resolve(__dirname, './ui-src'), path.resolve(__dirname, './ui-src/lib')],
    }, {
      test: /\.png$/,
      use: ['file-loader'],
    }, {
      test: /\.css$/,
      loader: 'style-loader!css-loader',
    }],
  },
  plugins: [
    new webpack.IgnorePlugin(/\.\/locale$/),
    new webpack.NormalModuleReplacementPlugin(
      /node_modules\/moment\/moment\.js$/,
      './min/moment.min.js'),
    new webpack.NormalModuleReplacementPlugin(
      /node_modules\/moment-timezone\/index\.js$/,
      './builds/moment-timezone-with-data-2012-2022.min.js'),
  ],
};
