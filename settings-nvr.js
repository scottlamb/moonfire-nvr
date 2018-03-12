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
