// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera digital video recorder.
// Copyright (C) 2018 Dolf Starreveld <dolf@starreveld.com>
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// In addition, as a special exception, the copyright holders give
// permission to link the code of portions of this program with the
// OpenSSL library under certain conditions as described in each
// individual source file, and distribute linked combinations including
// the two.
//
// You must obey the GNU General Public License in all respects for all
// of the code used other than OpenSSL. If you modify file(s) with this
// exception, you may extend this exception to your version of the
// file(s), but you are not obligated to do so. If you do not wish to do
// so, delete this exception statement from your version. If you delete
// this exception statement from all source files in the program, then
// also delete it here.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

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
