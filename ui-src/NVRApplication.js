// vim: set et sw=2:
//
// TODO: test abort.
// TODO: add error bar on fetch failure.
// TODO: style: no globals? string literals? line length? fn comments?
// TODO: live updating.

import 'jquery-ui/themes/base/button.css';
import 'jquery-ui/themes/base/core.css';
import 'jquery-ui/themes/base/datepicker.css';
import 'jquery-ui/themes/base/dialog.css';
import 'jquery-ui/themes/base/resizable.css';
import 'jquery-ui/themes/base/theme.css';
import 'jquery-ui/themes/base/tooltip.css';

// This causes our custom css to be loaded after the above!
require('./assets/index.css');

import $ from 'jquery';
import 'jquery-ui/ui/widgets/datepicker';
import 'jquery-ui/ui/widgets/dialog';
import 'jquery-ui/ui/widgets/tooltip';

import Camera from './lib/models/Camera';
import CameraView from './lib/views/CameraView';
import CalendarView from './lib/views/CalendarView';
import NVRSettingsView from './lib/views/NVRSettingsView';
import CheckboxGroupView from './lib/views/CheckboxGroupView';
import RecordingFormatter from './lib/support/RecordingFormatter';
import TimeFormatter, {
  TimeStamp90kFormatter,
} from './lib/support/TimeFormatter';
import MoonfireAPI from './lib/MoonfireAPI';

const api = new MoonfireAPI();
let cameraViews = null; // CameraView objects
let calendarView = null; // CalendarView object

// IANA timezone name.
let timeFmt = 'YYYY-MM-DD HH:mm:ss';
let timeFormatter = null;
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
  console.log('Forming video url');
  const url = api.videoPlayUrl(
    camera.uuid,
    recording,
    trimmedRange,
    nvrSettingsView.timeStampTrack
  );
  console.log('Video url: ' + url);
  const video = $('<video controls preload="auto" autoplay="true" />');
  const dialog = $('<div class="playdialog" />').append(video);
  console.log('have dialog');
  $('body').append(dialog);
  console.log('appended dialog');

  let [formattedStart, formattedEnd] = timeFormatter90k.formatSameDayShortened(
    trimmedRange.startTime90k,
    trimmedRange.endTime90k
  );
  console.log('range: ' + formattedStart + '-' + formattedEnd);
  dialog.dialog({
    title: camera.shortName + ', ' + formattedStart + ' to ' + formattedEnd,
    width: recording.videoSampleEntryWidth / 4,
    close: function() {
      dialog.remove();
    },
  });
  // Now that dialog is up, set the src so video starts
  console.log('Video setting rc: ', url);
  video.attr('src', url);
}

/**
 * Fetch camera view data for a given date/time range.
 *
 * @param  {Range90k} selectedRange Desired time range
 * @param  {Number} videoLength Desired length of video segments
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
    if (url === cameraView.recordingsUrl) {
      /*
       * @TODO: Can this actually happen?
       */
      continue; // already in progress, nothing to do.
    }
    if (cameraView.recordingsReq !== null) {
      /*
       * @TODO: Aborting here does not see right.
       * If there is another request, it would be because settings changed
       * and so an abort would leave the UI in a possible inconcistent state.
       */
      cameraView.recordingsReq.abort();
    }
    cameraView.delayedShowLoading(500);
    let r = api.request(url);
    cameraView.recordingsUrl = url;
    cameraView.recordingsReq = r;
    r.always(function() {
      cameraView.recordingsReq = null;
    });
    r
      .then(function(data, status, req) {
        // Sort recordings in descending order.
        data.recordings.sort(function(a, b) {
          return b.startId - a.startId;
        });
        console.log('Fetched results > updating recordings');
        cameraView.recordingsJSON = data.recordings;
      })
      .catch(function(data, status, err) {
        console.log(url, ' load failed: ', status, ': ', err);
      });
  }
}

/**
 * Setup the calendar for use.
 *
 * A CalendarView is established as necessary and then it is initialized
 * from the camera views. This allows the view to determine what days/dates
 * are involved and configure datepickers etc.
 *
 * This should only change when a new cameras load happens and this function
 * should be called again.
 *
 * We also setup a handler for when the view indicates the user selectable
 * date range had changed. This is used to fetch new detailed data.
 *
 * @param  {Iterable} views     Camera views
 * @param  {Number} videoLength Desired length of video segments
 * @return {CalendarView}       The camera view to be used
 */
function setupCalendar(views) {
  calendarView.initializeWith(views);
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
 * @param  {[type]} data [description]
 * @return {[type]}      [description]
 */
function onReceivedCameras(data) {
  newTimeZone(data.timeZoneName);

  // Set up controls and values
  const nvrSettingsView = new NVRSettingsView();
  nvrSettingsView.onVideoLengthChange = (vl) =>
    fetch(calendarView.selectedRange, vl);
  nvrSettingsView.onTimeFormatChange = (format) => {
    cameraViews.forEach((view) => (view.timeFormat = format));
  };
  nvrSettingsView.onTrimChange = (t) => {
    console.log('Trim handler');
    const newTrim = e.target.checked;
    cameraViews.forEach((view) => (view.trimmed = newTrim));
  };
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
        console.log('NVR load error: ', status, err);
        onReceivedCameras({cameras: []});
      })
      .catch((e) => {
        console.log('NVR load exception: ', e);
        onReceivedCameras({cameras: []});
      });
  }
}
