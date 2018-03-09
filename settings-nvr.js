// vim: set et ts=2 sw=2:
//

/**
 * This module must export a map, but can use a function with no arguments
 * that returns a map, or a function that receives the "env" and "args"
 * values from webpack.
 *
 * @type {Object}
 */
module.exports.settings = {
  // Project related: use ./ in front of project root relative files!
  app_src_dir: './ui-src',
  dist_dir: './ui-dist',

  // App related
  app_title: 'Moonfire NVR',

  // Where is the server to be found
  moonfire: {
    server: 'localhost',
    port: 8080,
  },

  /*
   * In settings override file you can add sections like below on this level.
   * After processing, anything defined in mode.production or mode.development,
   * as appropriate based on --mode argument to webpack, will be merged
   * into the top level of this settings module. This allows you to add to, or
   * override anything listed above.
   *
   * webpack_mode: {
   *  production: {},
   *  development: {},
  },
  */
};
