// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security stream digital video recorder.
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

import Range90k from './Range90k';

/**
 * Stream JSON wrapper.
 */
export default class Stream {
  /**
   * Construct from JSON.
   *
   * @param  {JSON} streamJson JSON for single stream.
   */
  constructor(streamJson) {
    this.json_ = streamJson;
  }

  /**
   * Get maximimum amount of storage allowed to be used for stream's video
   * samples.
   *
   * @return {Number} Amount in bytes
   */
  get retainBytes() {
    return this.json_.retainBytes;
  }

  /**
   * Get a Range90K object representing the range encompassing all available
   * video samples for the stream.
   *
   * This range does not mean every second of the range has video!
   *
   * @return {Range90k} The stream's available recordings range
   */
  get range90k() {
    return new Range90k(
      this.json_.minStartTime90k,
      this.json_.maxEndTime90k,
      this.json_.totalDuration90k
    );
  }

  /**
   * Get the total amount of storage currently taken up by the stream's video
   * samples.
   *
   * @return {Number} Amount in bytes
   */
  get totalSampleFileBytes() {
    return this.json_.totalSampleFileBytes;
  }

  /**
   * Get the list of the stream's days for which there are video samples.
   *
   * The result is a Map with dates as keys (in YYYY-MM-DD format) and each
   * value is a Range90k object for that day. Here too, the range does not
   * mean every second in the range has video, but presence of an entry for
   * a day does mean there is at least one (however short) video segment
   * available.
   *
   * @return {Map} Dates are keys, values are Range90K objects.
   */
  get days() {
    return new Map(
      Object.entries(this.json_.days).map(function(t) {
        let [k, v] = t;
        v = new Range90k(v.startTime90k, v.endTime90k, v.totalDuration90k);
        return [k, v];
      })
    );
  }
}
