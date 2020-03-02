// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
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

import $ from 'jquery';

/**
 * Class to control the view of NVR Settings.
 *
 * These  settings/controls include:
 * - Max video length
 * - Trim segment start/end
 * - Time Format
 */
export default class NVRSettingsView {
  /**
   * Construct based on element ids
   */
  constructor({
    videoLenId = 'split',
    trimCheckId = 'trim',
    tsTrackId = 'ts',
    timeFmtId = 'timefmt',
  } = {}) {
    this._ids = {videoLenId, trimCheckId, tsTrackId, timeFmtId};
    this._videoLength = null;
    this._videoLengthHandler = null;
    this._trim = null;
    this._trimHandler = null;
    this._timeFmtStr = null;
    this._timeFmtHandler = null;
    this._tsTrack = null;
    this._tsTrackHandler = null;
    this._wireControls();
  }

  /**
   * Find selected option in <select> and return value, or first option's value.
   *
   * The first option's value is returned if no option is selected.
   *
   * @param  {jQuery} selectEl jQuery element for the <select>
   * @return {String}          Value of the selected/first option
   */
  _findSelectedOrFirst(selectEl) {
    let value = selectEl.find(':selected').val();
    if (!value) {
      value = selectEl.find('option:first-child').val();
    }
    return value;
  }

  /**
   * Wire up all controls and handlers.
   *
   */
  _wireControls() {
    const videoLengthEl = $(`#${this._ids.videoLenId}`);
    this._videoLength = this._findSelectedOrFirst(videoLengthEl);
    videoLengthEl.change((e) => {
      const newValueStr = e.currentTarget.value;
      this._videoLength =
        newValueStr == 'infinite' ? Infinity : Number(newValueStr);
      if (this._videoLengthHandler) {
        this._videoLengthHandler(this._videoLength);
      }
    });

    const trimEl = $(`#${this._ids.trimCheckId}`);
    this._trim = trimEl.is(':checked');
    trimEl.change((e) => {
      this._trim = e.currentTarget.checked;
      if (this._trimHandler) {
        this._trimHandler(this._trim);
      }
    });

    const timeFmtEl = $(`#${this._ids.timeFmtId}`);
    this._timeFmtStr = this._findSelectedOrFirst(timeFmtEl);
    timeFmtEl.change((e) => {
      this._timeFmtStr = e.target.value;
      if (this._timeFmtHandler) {
        this._timeFmtHandler(this._timeFmtStr);
      }
    });

    const trackEl = $(`#${this._ids.tsTrackId}`);
    this._tsTrack = trackEl.is(':checked');
    trackEl.change((e) => {
      this._tsTrack = e.target.checked;
      if (this._tsTrackHandler) {
        this._tsTrackHandler(this._tsTrack);
      }
    });
  }

  /**
   * Get currently selected video length.
   *
   * @return {Number} Video length value
   */
  get videoLength() {
    return this._videoLength;
  }

  /**
   * Get currently selected time format string.
   *
   * @return {String} Format string
   */
  get timeFormatString() {
    return this._timeFmtStr;
  }

  /**
   * Get currently selected trim setting.
   *
   * @return {Boolean}
   */
  get trim() {
    return this._trim;
  }

  /**
   * Determine value of timestamp tracking option
   *
   * @return {Boolean}
   */
  get timeStampTrack() {
    return this._tsTrack;
  }

  /**
   * Set a handler to be called when the time format string changes.
   *
   * The handler will be called with one argument: the new format string.
   *
   * @param  {Function} handler Format change handler
   */
  set onTimeFormatChange(handler) {
    this._timeFmtHandler = handler;
  }

  /**
   * Set a handler to be called when video length popup changes.
   *
   * The handler will be called with one argument: the new video length.
   *
   * @param  {Function} handler Video Length change handler
   */
  set onVideoLengthChange(handler) {
    this._videoLengthHandler = handler;
  }

  /**
   * Set a handler to be called when video trim checkbox changes.
   *
   * The handler will be called with one argument: the new trim value (Boolean).
   *
   * @param  {Function} handler Trim change handler
   */
  set onTrimChange(handler) {
    this._trimHandler = handler;
  }

  /**
   * Set a handler to be called when video timestamp tracking checkbox changes.
   *
   * The handler will be called with one argument: the new tsTrack value
   * (Boolean).
   *
   * @param  {Function} handler Timestamp track change handler
   */
  set onTimeStampTrackChange(handler) {
    this._tsTrackHandler = handler;
  }
}
