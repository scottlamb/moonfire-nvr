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
import Range90k from '../models/Range90k';

/**
 * Class to encapsulate recording JSON data.
 */
export default class Recording extends JsonWrapper {
  /**
   * Accept JSON data to be encapsulated
   *
   * @param  {object} recordingJson JSON for a recording
   */
  constructor(recordingJson) {
    super(recordingJson);
  }

  /**
   * Get recording's startId.
   *
   * @return {String} startId for recording
   */
  get startId() {
    return this.json.startId;
  }

  /**
   * Get recording's endId.
   *
   * @return {String} endId for recording
   */
  get endId() {
    return this.json.endId;
  }

  /**
   * Return start time of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get startTime90k() {
    return this.json.startTime90k;
  }

  /**
   * Return end time of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get endTime90k() {
    return this.json.endTime90k;
  }

  /**
   * Return duration of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get duration90k() {
    const data = this.json;
    return data.endTime90k - data.startTime90k;
  }

  /**
   * Compute the range of the recording in 90k timestamp units,
   * optionally trimmed by another range.
   *
   * @param  {Range90k} trimmedAgainst Optional range to trim against
   * @return {Range90k}                Resulting range
   */
  range90k(trimmedAgainst = null) {
    let result = new Range90k(
      this.startTime90k,
      this.endTime90k,
      this.duration90k
    );
    return trimmedAgainst ? result.trimmed(trimmedAgainst) : result;
  }
  /**
   * Return duration of recording in seconds.
   * @return {Number} Time in units of seconds.
   */
  get duration() {
    return this.duration90k / 90000;
  }

  /**
   * Get the number of bytes used by sample storage.
   *
   * @return {Number} Total bytes used
   */
  get sampleFileBytes() {
    return this.json.sampleFileBytes;
  }

  /**
   * Get the number of video samples (frames) for the recording.
   *
   * @return {Number} Total bytes used
   */
  get frameCount() {
    return this.json.videoSamples;
  }

  /**
   * Get the has for the video samples.
   *
   * @return {String} Hash
   */
  get videoSampleEntryHash() {
    return this.json.videoSampleEntrySha1;
  }

  /**
   * Get the width of the frame(s) of the video samples.
   *
   * @return {Number} Width in pixels
   */
  get videoSampleEntryWidth() {
    return this.json.videoSampleEntryWidth;
  }

  /**
   * Get the height of the frame(s) of the video samples.
   *
   * @return {Number} Height in pixels
   */
  get videoSampleEntryHeight() {
    return this.json.videoSampleEntryHeight;
  }
}
