// vim: set et ts=2 sw=2:
//
const path = require('path');
const merge = require('webpack-merge');

/**
 * Helper function to require a file and catch errors so we can
 * distinguish between failure to find the module and errors in the
 * module.
 *
 * When a require results in errors (as opposed to the file not being
 * found), we throw an exception.
 *
 * If the module that is require-d is a function, it will be executed,
 * passing the "env" and "args" parameters from the settingsConfig to it.
 * The function should return a map.
 *
 * @param  {String} path            Path to be passed to require()
 * @param  {object} settingsConfig  Settings passed to new Settings()
 * @param  {Boolean} optional       True file not to exist
 * @return {object}                 The module, or {} if not found (optional)
 */
function requireHelper(path, settingsConfig, optional) {
  let module = {};
  try {
    require.resolve(path); // Throws if not found
    try {
      module = require(path);
      if (typeof(module) === 'function') {
        module = module(settingsConfig.env, settingsConfig.args);
      }
      // Get owned properties only: now a literal map
      module = Object.assign({}, require(path).settings);
    } catch (e) {
      throw new Error('Settings file (' + path + ') has errors.');
    }
  } catch (e) {
    if (!optional) {
      throw new Error('Settings file (' + path + ') not found.');
    }
  }
  const args = settingsConfig.args;
  const webpackMode = (args ? args.mode : null) || 'none';
  const modes = module.webpack_mode || {};
  delete module.webpack_mode; // Not modifying original module. We have a copy!
  if (webpackMode && modes) {
    module = merge(module, modes[webpackMode]);
  }
  return module;
}

/**
 * General purpose settings loading class.
 *
 * The class first reads a specified file extracting a map object with
 * settings. It then attempts to read a second file which, if successfull,
 * will be merged to override values from the first.
 *
 * The module exported in each file must either be a map, in which case
 * it is used directly, or a function with no arguments. In the latter case
 * it will be called in order to obtain the map.
 *
 * The intended use is that the first file contains project level settings
 * that are checked into a repository. The second file should be for local
 * (development) overrides and should not be checked in.
 *
 * If the primary file is allowed optional and is not found, we still
 * attempt to read the secondary, but it is never an error if that file
 * does not exist.
 *
 * Both primary and secondary files may contain a property called webpack_mode
 * that, in turn, may contain properties named "development" and
 * "production". During loading, if these properties are present, the whole
 * "webpack_mode" property is *NOT* delivered in the final result, but the
 * sub-property corresponding to webpack's "--mode" argument is merged
 * with the configuration object at the top-level. This allows either
 * sub-property to override defaults in the settings.
 *
 * Provide some convenience member variables in the Settings object:
 * settings_config {object} object with the arguments to the constructor
 * settings {object} The values map of the settings that were configured
 *
 * In many cases a user of this class will only be intersted in the values
 * component. A typical usage patterns would the be:
 * <pre><code>
 * const Settings = require('Settings');
 * const settings = (new Settings()).values;
 * </code></pre>
 *
 * This does make the "config" component of the Settings instance unavailable.
 * That can be remedied:
 * <pre><code>
 * const Settings = require('Settings');
 * const _settings = new Settings();
 * const settings = _settings.values;
 * </code></pre>
 *
 * Now the config is available as "_settings.config".
 *
 * @type {NVRSettings}
 */
class Settings {
  /**
   * Construct the settings object by attempting to read and merge
   * both files.
   *
   * Settings file and alternate or specified as filenames only. They
   * are always looked for in the project root directory.
   *
   * "env", and "args" options are intended to be passed in like so:
   * <pre><code>
   * const Settings = require('./Settings');
   *
   * module.exports = (env, args) => {
   *   const settingsObject = new Settings({ env: env, args: args });
   *   const settings = settingsObject.settings;
   *
   *   return {
   *   ... webpack config here, using things like
   *   ... settings.app_title
   *   };
   * }
   * </code></pre>
   *
   * The Settings object inspects "args.mode" to determine how to overload
   * some settings values, and defaults to 'none' if not present.
   * Alternatively, null can be passed for "env", and you could pass
   * <pre>{ mode: 'development' }</pre> for args (or use 'production').
   * Both values will be available later from  settingsObject.settings_config
   * and using the values from webpack gives full access to everything webpack
   * knows.
   *
   * @param  {Boolean} options.optional     True if main file is optional
   * @param  {String}  options.projectRoot  Path to project root
   * @param  {String}  options.primaryFile   Name of main settings file
   * @param  {String}  options.secondaryFile Name of secondary settings file
   * @param  {String}  options.env          Environment variables (from webpack)
   * @param  {String}  options.args         Arguments (from webpack)
   */
  constructor({
    optional = false,
    projectRoot = './',
    primaryFile = 'settings.js',
    secondaryFile = 'settings-local.js',
    env = null,
    args = null,
  } = {}) {
    if (!projectRoot) {
      throw new Error('projectRoot argument for Settings is not set.');
    }

    // Remember settings, as provided
    // eslint-disable-next-line prefer-rest-params
    this.settings_config = arguments[0];

    // Convert settings file names into absolute paths.
    const primaryPath = path.resolve(projectRoot, primaryFile);
    const secondaryPath = path.resolve(projectRoot, secondaryFile);

    // Check if we can resolve the primary file and if we can, require it.
    const _settings =
      requireHelper(primaryPath, this.settings_config, optional);

    // Merge secondary override file, if it exists
    this.settings = merge(_settings,
      requireHelper(secondaryPath, this.settings_config, true));
  };

  /**
   * Take one or more webpack configurations and merge them.
   *
   * This uses the webpack-merge functionality, but each argument is subjected
   * to some pre-processing.
   * - If the argument is a string, a 'require' is performed with it first
   * - If the remaining value is a function, it is expected to be like a
   *   webpack initialization function which gets passed "env" and "args"
   *   and it is called like that.
   * - The remaining value is fed to webpack-merge.
   *
   * @param  {[object]} webpackConfig1 Object representing the config
   * @return {[type]}                  Merged configuration
   */
  webpackMerge(...packs) {
    const unpack = (webpackConfig) => {
      if ((typeof(webpackConfig) === 'string') ||
        (webpackConfig instanceof String)) {
        webpackConfig = require(webpackConfig);
      }
      const config = this.settings_config;
      if (typeof(webpackConfig) === 'function') {
        return webpackConfig(config.env, config.args);
      }
      return webpackConfig;
    };

    return merge(packs.map((p) => unpack(p)));
  }
}

module.exports = Settings;
