// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018-2020 The Moonfire NVR Authors
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
import CalendarView from './lib/views/CalendarView';
import VideoDialogView from './lib/views/VideoDialogView';
import NVRSettingsView from './lib/views/NVRSettingsView';
import RecordingFormatter from './lib/support/RecordingFormatter';
import StreamSelectorView from './lib/views/StreamSelectorView';
import StreamView from './lib/views/StreamView';
import TimeFormatter from './lib/support/TimeFormatter';
import TimeStamp90kFormatter from './lib/support/TimeStamp90kFormatter';
import MoonfireAPI from './lib/MoonfireAPI';

const api = new MoonfireAPI();
let streamViews = null; // StreamView objects
let calendarView = null; // CalendarView object
let loginDialog = null;

/**
 * Currently selected time format specification.
 *
 * @type {String}
 */
const timeFmt = 'YYYY-MM-DD HH:mm:ss';

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
 * @param  {String} streamType "main" or "sub"
 * @param  {object} range Range Object
 * @param  {object} recording Recording object
 * @return {void}
 */
function onSelectVideo(nvrSettingsView, camera, streamType, range, recording) {
  console.log('Recording clicked: ', recording);
  const trimmedRange = recording.range90k(nvrSettingsView.trim ? range : null);
  const url = api.videoPlayUrl(
      camera.uuid,
      streamType,
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
  let width = recording.videoSampleEntryWidth *
              recording.videoSampleEntryPaspHSpacing /
              recording.videoSampleEntryPaspVSpacing;
  const maxWidth = window.innerWidth * 3 / 4;
  while (width > maxWidth) {
    width /= 2;
  }
  new VideoDialogView()
      .attach($('body'))
      .play(videoTitle, width, url);
}

/**
 * Fetch stream view data for a given date/time range.
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
  for (const streamView of streamViews) {
    const url = api.recordingsUrl(
        streamView.camera.uuid,
        streamView.streamType,
        selectedRange.startTime90k,
        selectedRange.endTime90k,
        videoLength
    );
    if (streamView.recordingsReq !== null) {
      /*
       * If there is another request, it would be because settings changed
       * and so an abort is to make room for this new request, now necessary
       * for the changed situation.
       */
      streamView.recordingsReq.abort();
    }
    streamView.delayedShowLoading(500);
    const r = api.request(url);
    streamView.recordingsUrl = url;
    streamView.recordingsReq = r;
    streamView.recordingsRange = selectedRange.range90k();
    r.always(function() {
      streamView.recordingsReq = null;
    });
    r
        .then(function(data /* , status, req */) {
        // Sort recordings in descending order.
          data.recordings.sort(function(a, b) {
            return b.startId - a.startId;
          });
          console.log(
              'Fetched results for "%s-%s" > updating recordings',
              streamView.camera.shortName, streamView.streamType
          );
          streamView.recordingsJSON = data;
        })
        .catch(function(data, status, err) {
          console.error(url, ' load failed: ', status, ': ', err);
        });
  }
}

/**
 * Updates the session bar at the top of the page.
 *
 * @param  {Object} session the "session" key of the main API request's JSON,
 *         or null.
 */
function updateSession(session) {
  const sessionBar = $('#session');
  sessionBar.empty();
  if (session === null || session === undefined) {
    sessionBar.hide();
    return;
  }
  sessionBar.append($('<span id="session-username" />').text(session.username));
  const logout = $('<a>logout</a>');
  logout.click(() => {
    api
        .logout(session.csrf)
        .done(() => {
          onReceivedTopLevel(null);
          loginDialog.dialog('open');
        });
  });
  sessionBar.append(' | ', logout);
  sessionBar.show();
}

/**
 * Initialize the page after receiving top-level data.
 *
 * Sets the following globals:
 * zone - timezone from data received
 * streamViews - array of views, one per stream
 *
 * Builds the dom for the left side controllers
 *
 * @param  {Object} data JSON resulting from the main API request /api/?days=
 *         or null if the request failed.
 */
function onReceivedTopLevel(data) {
  if (data === null) {
    data = {cameras: [], timeZoneName: null};
  }

  newTimeZone(data.timeZoneName);
  updateSession(data.session);

  // Set up controls and values
  const nvrSettingsView = new NVRSettingsView();
  nvrSettingsView.onVideoLengthChange = (vl) =>
    fetch(calendarView.selectedRange, vl);
  nvrSettingsView.onTimeFormatChange = (format) =>
    streamViews.forEach((view) => (view.timeFormat = format));
  nvrSettingsView.onTrimChange = (t) =>
    streamViews.forEach((view) => (view.trimmed = t));
  newTimeFormat(nvrSettingsView.timeFormatString);

  calendarView = new CalendarView({timeZone: timeFormatter.tz});
  calendarView.onRangeChange = (selectedRange) =>
    fetch(selectedRange, nvrSettingsView.videoLength);

  const streamsParent = $('#streams');
  const videos = $('#videos');

  streamsParent.empty();
  videos.empty();

  streamViews = [];
  const streamSelectorCameras = [];
  for (const cameraJson of data.cameras) {
    const camera = new Camera(cameraJson);
    const cameraStreams = {};
    Object.keys(camera.streams).forEach((streamType) => {
      const sv = new StreamView(
          camera,
          streamType,
          new RecordingFormatter(timeFormatter.formatStr, timeFormatter.tz),
          nvrSettingsView.trim,
          videos);
      sv.onRecordingClicked = (recordingModel) => {
        console.log('Recording clicked', recordingModel);
        onSelectVideo(
            nvrSettingsView,
            camera,
            streamType,
            calendarView.selectedRange,
            recordingModel
        );
      };
      streamViews.push(sv);
      cameraStreams[streamType] = sv;
    });
    streamSelectorCameras.push({
      camera: camera,
      streamViews: cameraStreams,
    });
  };

  // Create stream enable checkboxes
  const streamSelector =
      new StreamSelectorView(streamSelectorCameras, streamsParent);
  streamSelector.onChange = () => calendarView.initializeWith(streamViews);
  calendarView.initializeWith(streamViews);

  console.log('Loaded: ' + streamViews.length + ' stream views');
}

/**
 * Handles the submit action on the login form.
 */
function sendLoginRequest() {
  if (loginDialog.pending) {
    return;
  }

  const username = $('#login-username').val();
  const password = $('#login-password').val();
  const submit = $('#login-submit');
  const error = $('#login-error');

  error.empty();
  error.removeClass('ui-state-highlight');
  submit.button('option', 'disabled', true);
  loginDialog.pending = true;
  console.info('logging in as', username);
  api
      .login(username, password)
      .done(() => {
        console.info('login successful');
        loginDialog.dialog('close');
        sendTopLevelRequest();
      })
      .catch((e) => {
        console.info('login failed:', e);
        error.show();
        error.addClass('ui-state-highlight');
        error.text(e.responseText);
      })
      .always(() => {
        submit.button('option', 'disabled', false);
        loginDialog.pending = false;
      });
}

/** Sends the top-level api request. */
function sendTopLevelRequest() {
  api
      .request(api.nvrUrl(true))
      .done((data) => onReceivedTopLevel(data))
      .catch((e) => {
        console.error('NVR load exception: ', e);
        onReceivedTopLevel(null);
        if (e.status == 401) {
          loginDialog.dialog('open');
        }
      });
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
    loginDialog = $('#login').dialog({
      autoOpen: false,
      modal: true,
      buttons: [
        {
          id: 'login-submit',
          text: 'Login',
          click: sendLoginRequest,
        },
      ],
    });
    loginDialog.pending = false;
    loginDialog.find('form').on('submit', function(event) {
      event.preventDefault();
      sendLoginRequest();
    });
    sendTopLevelRequest();
  }
}
