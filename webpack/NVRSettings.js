// vim: set et ts=2 sw=2:
//
const path = require('path');
const Settings = require('./parts/Settings');

/**
 * Exports a sub-class of Settings specifically for the Moonfire NVR project.
 *
 * Gives us a simpler constructor that encapsulates the names of the expected
 * settings files.
 *
 * Provide some convenience member variables:
 * config {object} Map of the original settings configuration
 * values {object} The values map of the settings that were configured
 *
 * @type {NVRSettings}
 */
module.exports = class NVRSettings extends Settings {
  /**
   * Construct an NVRSettings object.
   *
   * This object will be a subclass of Settings, with some extra functionality.
   *
   * Initializes the super Settings object with the proper project root
   * and named settings files.
   *
   *  @param {object} env        "env" object passed to webpack config function
   *  @param {object} args       "args" object passed to webpack config function
   * @param  {String} projectRoot Project root, defaults to '.' which is
   *                              usually the directory from which you run
   *                              npm or yarn.
   */
  constructor(env, args, projectRoot = './') {
    super({
      projectRoot: path.resolve(projectRoot),
      primaryFile: 'settings-nvr.js',
      secondaryFile: 'settings-nvr-local.js',
      env: env,
      args: args,
    });
    const config = this.settings_config;
    // Add some absolute paths that might be relevant
    this.settings = Object.assign(this.settings, {
      _paths: {
        project_root: config.projectRoot,
        app_src_dir: path.join(config.projectRoot, this.settings.app_src_dir),
        dist_dir: path.join(config.projectRoot, this.settings.dist_dir),
      },
    });
  }
};
