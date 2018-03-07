// vim: set et sw=2:
//

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
   * @return {Boolean} True if view is enabled
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
