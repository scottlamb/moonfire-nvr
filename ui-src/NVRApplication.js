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

// TODO: test abort.
// TODO: add error bar on fetch failure.
// TODO: live updating.

import $ from 'jquery';

// tooltip needs:
// css.structure: ../../themes/base/core.css
// css.structure: ../../themes/base/tooltip.css
// css.theme: ../../themes/base/theme.css

import 'jquery-ui/themes/base/core.css';
import 'jquery-ui/themes/base/tooltip.css';
import 'jquery-ui/themes/base/theme.css';

// This causes our custom css to be loaded after the above!
import './assets/index.css';

// Get ui widgets themselves
import 'jquery-ui/ui/widgets/tooltip';

import Camera from './lib/models/Camera';
import CameraView from './lib/views/CameraView';
import CalendarView from './lib/views/CalendarView';
import VideoDialogView from './lib/views/VideoDialogView';
import NVRSettingsView from './lib/views/NVRSettingsView';
import CheckboxGroupView from './lib/views/CheckboxGroupView';
import RecordingFormatter from './lib/support/RecordingFormatter';
import TimeFormatter from './lib/support/TimeFormatter';
import TimeStamp90kFormatter from './lib/support/TimeStamp90kFormatter';
import MoonfireAPI from './lib/MoonfireAPI';

const api = new MoonfireAPI();
let cameraViews = null; // CameraView objects
let calendarView = null; // CalendarView object

/**
 * Currently selected time format specification.
 *
 * @type {String}
 */
let timeFmt = 'YYYY-MM-DD HH:mm:ss';

/**
 * Currently active time formatter.
 * This is lazy initialized at the point we receive the timezone information
 * and never changes afterwards, except possibly for changing the timezone.
 *
 * @type {[type]}
 */
let timeFormatter = null;

/**
 * Currently active time formatter for internal time format.
 * This is lazy initialized at the point we receive the timezone information
 * and never changes afterwards, except possibly for changing the timezone.
 *
 * @type {[type]}
 */
let timeFormatter90k = null;

/**
 * Globally set a new timezone for the app.
 *
 * @param  {String} timeZone Timezone name
 */
function newTimeZone(timeZone) {
  timeFormatter = new TimeFormatter(timeFmt, timeZone);
  timeFormatter90k = new TimeStamp90kFormatter(timeZone);
}

/**
 * Globally set a new time format for the app.
 *
 * @param  {String} format Time format specification
 */
function newTimeFormat(format) {
  timeFormatter = new TimeFormatter(format, timeFormatter.tz);
}

/**
 * Event handler for clicking on a video.
 *
 * A 'dialog' object is attached to the body of the dom and it
 * properly initialized with the corrcet src url.
 *
 * @param  {NVRSettings} nvrSettingsView NVRSettingsView in effect
 * @param  {object} camera Object for the camera
 * @param  {object} range Range Object
 * @param  {object} recording Recording object
 * @return {void}
 */
function onSelectVideo(nvrSettingsView, camera, range, recording) {
  console.log('Recording clicked: ', recording);
  const trimmedRange = recording.range90k(nvrSettingsView.trim ? range : null);
  const url = api.videoPlayUrl(
    camera.uuid,
    recording,
    trimmedRange,
    nvrSettingsView.timeStampTrack
  );
  const [
    formattedStart,
    formattedEnd,
  ] = timeFormatter90k.formatSameDayShortened(
    trimmedRange.startTime90k,
    trimmedRange.endTime90k
  );
  const videoTitle =
    camera.shortName + ', ' + formattedStart + ' to ' + formattedEnd;
  new VideoDialogView()
    .attach($('body'))
    .play(videoTitle, recording.videoSampleEntryWidth / 4, url);
}

/**
 * Fetch camera view data for a given date/time range.
 *
 * @param  {Range90k} selectedRange Desired time range
 * @param  {Number} videoLength Desired length of video segments, or Infinity
 */
function fetch(selectedRange, videoLength) {
  if (selectedRange.startTime90k === null) {
    return;
  }
  console.log(
    'Fetching> ' +
      selectedRange.formatTimeStamp90k(selectedRange.startTime90k) +
      ' to ' +
      selectedRange.formatTimeStamp90k(selectedRange.endTime90k)
  );
  for (let cameraView of cameraViews) {
    let url = api.recordingsUrl(
      cameraView.camera.uuid,
      selectedRange.startTime90k,
      selectedRange.endTime90k,
      videoLength
    );
    if (cameraView.recordingsReq !== null) {
      /*
       * If there is another request, it would be because settings changed
       * and so an abort is to make room for this new request, now necessary
       * for the changed situation.
       */
      cameraView.recordingsReq.abort();
    }
    cameraView.delayedShowLoading(500);
    let r = api.request(url);
    cameraView.recordingsUrl = url;
    cameraView.recordingsReq = r;
    cameraView.recordingsRange = selectedRange.range90k();
    r.always(function() {
      cameraView.recordingsReq = null;
    });
    r
      .then(function(data /* , status, req */) {
        // Sort recordings in descending order.
        data.recordings.sort(function(a, b) {
          return b.startId - a.startId;
        });
        console.log(
          'Fetched results for "%s" > updating recordings',
          cameraView.camera.shortName
        );
        cameraView.recordingsJSON = data.recordings;
      })
      .catch(function(data, status, err) {
        console.error(url, ' load failed: ', status, ': ', err);
      });
  }
}

/**
 * Initialize the page after receiving camera data.
 *
 * Sets the following globals:
 * zone - timezone from data received
 * cameraViews - array of views, one per camera
 *
 * Builds the dom for the left side controllers
 *
 * @param  {Object} data JSON resulting from the main API request /api/?days=
 */
function onReceivedCameras(data) {
  newTimeZone(data.timeZoneName);

  // Set up controls and values
  const nvrSettingsView = new NVRSettingsView();
  nvrSettingsView.onVideoLengthChange = (vl) =>
    fetch(calendarView.selectedRange, vl);
  nvrSettingsView.onTimeFormatChange = (format) =>
    cameraViews.forEach((view) => (view.timeFormat = format));
  nvrSettingsView.onTrimChange = (t) =>
    cameraViews.forEach((view) => (view.trimmed = t));
  newTimeFormat(nvrSettingsView.timeFormatString);

  calendarView = new CalendarView({timeZone: timeFormatter.tz});
  calendarView.onRangeChange = (selectedRange) =>
    fetch(selectedRange, nvrSettingsView.videoLength);

  const camerasParent = $('#cameras');
  const videos = $('#videos');

  cameraViews = data.cameras.map((cameraJson) => {
    const camera = new Camera(cameraJson);
    const cv = new CameraView(
      camera,
      new RecordingFormatter(timeFormatter.formatStr, timeFormatter.tz),
      nvrSettingsView.trim,
      videos
    );
    cv.onRecordingClicked = (recordingModel) => {
      console.log('Recording clicked', recordingModel);
      onSelectVideo(
        nvrSettingsView,
        camera,
        calendarView.selectedRange,
        recordingModel
      );
    };
    return cv;
  });

  // Create camera enable checkboxes
  const cameraCheckBoxes = new CheckboxGroupView(
    cameraViews.map((cv) => ({
      id: cv.camera.uuid,
      checked: true,
      text: cv.camera.shortName,
      camView: cv,
    })),
    camerasParent
  );
  cameraCheckBoxes.onCheckChange = (groupEl) => {
    groupEl.camView.enabled = groupEl.checked;
    calendarView.initializeWith(cameraViews);
  };

  calendarView.initializeWith(cameraViews);

  console.log('Loaded: ' + cameraViews.length + ' camera views');
}

/**
 * Class representing the entire application.
 */
export default class NVRApplication {
  /**
   * Construct the application object.
   */
  constructor() {}

  /**
   * Start the application.
   */
  start() {
    api
      .request(api.nvrUrl(true))
      .done((data) => onReceivedCameras(data))
      .fail((req, status, err) => {
        console.error('NVR load error: ', status, err);
        onReceivedCameras({cameras: []});
      })
      .catch((e) => {
        console.error('NVR load exception: ', e);
        onReceivedCameras({cameras: []});
      });
  }
}
