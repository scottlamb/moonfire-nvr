// vim: set et sw=2:
//

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
      this._videoLength = Number(e.target.value);
      if (this._videoLengthHandler) {
        this._videoLengthHandler(this._videoLength);
      }
    });

    const trimEl = $(`#${this._ids.trimCheckId}`);
    this._trim = trimEl.is(':checked');
    trimEl.change((e) => {
      this._trim = e.target.checked;
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
   * @return {Boolean} Trim setting.
   */
  get trim() {
    return this._trim;
  }

  /**
   * Determine value of timestamp tracking option
   *
   * @return {Boolean} True if tracking desired
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
