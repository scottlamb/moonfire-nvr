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

import JsonWrapper from './JsonWrapper';
import Stream from './Stream';

/**
 * Camera JSON wrapper.
 */
export default class Camera extends JsonWrapper {
  /**
   * Construct from JSON.
   *
   * @param  {JSON} cameraJson JSON for single camera.
   */
  constructor(cameraJson) {
    super(cameraJson);
    this.streams_ = {};
    Object.keys(cameraJson.streams).forEach((streamType) => {
      this.streams_[streamType] = new Stream(cameraJson.streams[streamType]);
    });
  }

  /** @return {String} */
  get uuid() {
    return this.json.uuid;
  }

  /** @return {String} */
  get shortName() {
    return this.json.shortName;
  }

  /** @return {String} */
  get description() {
    return this.json.description;
  }

  /** @return {Object.<string, Stream>} */
  get streams() {
    return this.streams_;
  }
}
