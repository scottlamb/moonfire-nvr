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

import RecordingsView from './RecordingsView';

/**
 * Class handling a camer view.
 *
 * A camera view consists of a list of available recording segments for
 * playback.
 */
export default class CameraView {
  /**
   * Construct the view.
   *
   * @param  {Camera} cameraModel        Model object for camera
   * @param  {[type]} recordingFormatter Formatter to be used by recordings
   * @param  {[type]} trimmed            True if rec. ranges should be trimmed
   * @param  {[type]} recordingsParent   Parent element to attach to or null)
   */
  constructor(
    cameraModel,
    recordingFormatter,
    trimmed,
    recordingsParent = null
  ) {
    this.camera = cameraModel;
    this.recordingsView = new RecordingsView(
      this.camera,
      recordingFormatter,
      trimmed,
      recordingsParent
    );
    this._enabled = true;
    this.recordingsUrl = null;
    this.recordingsReq = null;
  }

  /**
   * Get whether the view is enabled or not.
   *
   * @return {Boolean}
   */
  get enabled() {
    return this._enabled;
  }

  /**
   * Change enabled state of the view.
   *
   * @param  {Boolean} enabled Whether view should be enabled
   */
  set enabled(enabled) {
    this._enabled = enabled;
    this.recordingsView.show = enabled;
    console.log(
      'Camera ',
      this.camera.shortName,
      this.enabled ? 'enabled' : 'disabled'
    );
  }

  /**
   * Get the currently remembered recordings range for this camera.
   *
   * This is just passed on to the recordings view.
   *
   * @return {Range90k} Currently remembered range
   */
  get recordingsRange() {
    return this.recordingsView.recordingsRange;
  }

  /**
   * Set the recordings range for this view.
   *
   * This is just passed on to the recordings view.
   *
   * @param  {Range90k} range90k Range to remember
   */
  set recordingsRange(range90k) {
    this.recordingsView.recordingsRange = range90k;
  }

  /**
   * Set whether loading indicator should be shown or not.
   *
   * This indicator is really on the recordings list.
   *
   * @param  {Boolean} show True if indicator should be showing
   */
  set showLoading(show) {
    this.recordingsView.showLoading = show;
  }

  /**
   * Show the loading indicated after a delay, unless the timer has been
   * cleared already.
   *
   * @param  {Number} timeOutMs Delay (in ms) before indicator should appear
   */
  delayedShowLoading(timeOutMs) {
    this.recordingsView.delayedShowLoading(timeOutMs);
  }

  /**
   * Set new recordings from JSON data.
   *
   * @param  {Object} dataJSON JSON data (array)
   */
  set recordingsJSON(dataJSON) {
    this.recordingsView.recordingsJSON = dataJSON;
  }

  /**
   * Set a new time format string for the recordings list.
   *
   * @param  {String} formatStr Formatting string
   */
  set timeFormat(formatStr) {
    this.recordingsView.timeFormat = formatStr;
  }

  /**
   * Set the trimming option of the cameraview as desired.
   *
   * This is really just passed on to the recordings view.
   *
   * @param  {Boolean} enabled True if trimming should be enabled
   */
  set trimmed(enabled) {
    this.recordingsView.trimmed = enabled;
  }

  /**
   * Set a handler for clicks on a recording.
   *
   * The handler will be called with one argument, the recording model.
   *
   * @param  {Function} h Handler function
   */
  set onRecordingClicked(h) {
    this.recordingsView.onRecordingClicked = h;
  }
}
