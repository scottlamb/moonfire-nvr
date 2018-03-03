// vim: set et sw=2:

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

import $ from 'jquery';
import 'jquery-ui/ui/widgets/datepicker';
import 'jquery-ui/ui/widgets/dialog';
import 'jquery-ui/ui/widgets/tooltip';
import moment from 'moment-timezone';

const apiUrl = '/api/';

// IANA timezone name.
let zone = null;

// A dict describing the currently selected range.
let selectedRange = {
    startDateStr: null,  // null or YYYY-MM-DD
    startTimeStr: '',    // empty or HH:mm[:ss[:FFFFF]][+-HHmm]
    startTime90k: null,  // null or 90k units since epoch
    endDateStr: null,    // null or YYYY-MM-DD
    endTimeStr: '',      // empty or HH:mm[:ss[:FFFFF]][+-HHmm]
    endTime90k: null,    // null or 90k units since epoch
    singleDateStr: null, // if startDateStr===endDateStr, that value, otherwise null
};

// Cameras is a dictionary as retrieved from apiUrl + some extra props:
// * "enabled" is a boolean indicating if the camera should be displayed and
//   if it should be used to constrain the datepickers.
// * "recordingsUrl" is null or the currently fetched/fetching .../recordings url.
// * "recordingsRange" is a null or a dict (in the same format as
//   selectedRange) describing what is fetching/fetched.
// * "recordingsData" is null or the data fetched from "recordingsUrl".
// * "recordingsReq" is null or a jQuery ajax object of an active .../recordings
//   request if currently fetching.
let cameras = null;

function req(url) {
  return $.ajax(url, {
    dataType: 'json',
    headers: {'Accept': 'application/json'},
  });
}

/**
 * Produces a human-readable timestamp in 90k units.
 *
 * The format is ISO-8601 plus an extra colon component after the seconds
 * indicating the fractional time in 90,000ths of a second.
 *
 * The timestamp in seconds would be ts90k / 90000, or in ms it would
 * be ts90k / 90. We use "moment" for formatting, but since it takes only
 * ms resolution (truncated) we have to be smarter than that to handle
 * additional precision provided by the timestamp, if the format indicates such
 * preision is requested.
 *
 * We accept a format string in every way compatible with those accepted
 * by moment's format function, except that we treat the sub-second format
 * special and extend the functionality to work correctly for precisions up
 * to 6 decimals (micro-seconds). The default format uses 5 positions.
 *
 * @param {int} ts90k timestamp in 90,000ths of a second resolution
 * @param {string} format Format string
 * @return {string}
 */
function formatTime(ts90k, format = 'YYYY-MM-DDTHH:mm:ss.SSSSSZ') {
  const ms = ts90k / 90.0;
  let msRound = 0.5;
  // Check if format string requires subsecond precision beyond ms
  const matches = format.match(/(S+)/);
  if (matches && (matches[1].length > 3)) {
    // Determine portion not handled by moment, which uses ms
    const fracms = ts90k % 90000; // Fractional ms, 90k units
    console.log('fracms: ' + fracms);
    if (fracms >= 0.001) { // Nothing extra needed if fraction is < 1 µs
      const subFmt = matches[1];
      const extraLen = Math.min(subFmt.length - 3, 3); // µs precision
      const fracµs = fracms / 0.09 + // µs units
        (0.0 * Math.pow(10, (3 - extraLen))); // and rounding
      const newSubFmt = subFmt.substr(0, 3) +
        String(1000000 + fracµs).substr(4, extraLen);
      format = format.replace(subFmt, newSubFmt);
      msRound = 0;
    }
  }
  return moment.tz(ms + msRound, zone).format(format);
}


function onSelectVideo(camera, range, recording) {
  let url = apiUrl + 'cameras/' + camera.uuid + '/view.mp4?s=' + recording.startId;
  if (recording.endId !== undefined) {
    url += '-' + recording.endId;
  }
  const trim = $("#trim").prop("checked");
  let rel = '';
  let startTime90k = recording.startTime90k;
  if (trim && recording.startTime90k < range.startTime90k) {
    rel += range.startTime90k - recording.startTime90k;
    startTime90k = range.startTime90k;
  }
  rel += '-';
  let endTime90k = recording.endTime90k;
  if (trim && recording.endTime90k > range.endTime90k) {
    rel += range.endTime90k - recording.startTime90k;
    endTime90k = range.endTime90k;
  }
  if (rel !== '-') {
    url += '.' + rel;
  }
  if ($("#ts").prop("checked")) {
    url += '&ts=true';
  }
  console.log('Displaying video: ', url);
  let video = $('<video controls preload="auto" autoplay="true"/>');
  let dialog = $('<div class="playdialog"/>').append(video);
  $("body").append(dialog);

  // Format start and end times for the dialog title. If they're the same day,
  // abbreviate the end time.
  let formattedStart = formatTime(startTime90k);
  let formattedEnd = formatTime(endTime90k);
  let timePos = 'YYYY-mm-ddT'.length;
  if (formattedEnd.startsWith(formattedStart.substr(0, timePos))) {
    formattedEnd = formattedEnd.substr(timePos);
  }
  dialog.dialog({
      title: camera.shortName + ", " + formattedStart + " to " + formattedEnd,
      width: recording.videoSampleEntryWidth / 4,
      close: function() { dialog.remove(); },
  });
  video.attr("src", url);
}

function formatRecordings(camera) {
  let tbody = $("#tab-" + camera.uuid);
  $(".loading", tbody).hide();
  $(".r", tbody).remove();
  const frameRateFmt = new Intl.NumberFormat([], {maximumFractionDigits: 0});
  const sizeFmt = new Intl.NumberFormat([], {maximumFractionDigits: 1});
  const trim = $("#trim").prop("checked");
  for (let recording of camera.recordingsData.recordings) {
    const duration = (recording.endTime90k - recording.startTime90k) / 90000;
    let row = $('<tr class="r"/>');
    const startTime90k = trim && recording.startTime90k < camera.recordingsRange.startTime90k
        ? camera.recordingsRange.startTime90k : recording.startTime90k;
    const endTime90k = trim && recording.endTime90k > camera.recordingsRange.endTime90k
        ? camera.recordingsRange.endTime90k : recording.endTime90k;
    let formattedStart = formatTime(startTime90k);
    let formattedEnd = formatTime(endTime90k);
    const singleDateStr = camera.recordingsRange.singleDateStr;
    if (singleDateStr !== null && formattedStart.startsWith(singleDateStr)) {
      formattedStart = formattedStart.substr(11);
    }
    if (singleDateStr !== null && formattedEnd.startsWith(singleDateStr)) {
      formattedEnd = formattedEnd.substr(11);
    }
    row.append(
        $("<td/>").text(formattedStart),
        $("<td/>").text(formattedEnd),
        $("<td/>").text(recording.videoSampleEntryWidth + "x" + recording.videoSampleEntryHeight),
        $("<td/>").text(frameRateFmt.format(recording.videoSamples / duration)),
        $("<td/>").text(sizeFmt.format(recording.sampleFileBytes  / 1048576) + " MB"),
        $("<td/>").text(sizeFmt.format(recording.sampleFileBytes / duration * .000008) + " Mbps"));
    row.on("click", function() { onSelectVideo(camera, camera.recordingsRange, recording); });
    tbody.append(row);
  }
};

function reselectDateRange(startDateStr, endDateStr) {
  selectedRange.startDateStr = startDateStr;
  selectedRange.endDateStr = endDateStr;
  selectedRange.startTime90k = parseDateTime(startDateStr, selectedRange.startTimeStr, false);
  selectedRange.endTime90k = parseDateTime(endDateStr, selectedRange.endTimeStr, true);
  fetch();
}

// Run when selectedRange is populated/changed or when split changes.
function fetch() {
  console.log('Fetching ', formatTime(selectedRange.startTime90k), ' to ',
              formatTime(selectedRange.endTime90k));
  let split = $("#split").val();
  for (let camera of cameras) {
    let url = apiUrl + 'cameras/' + camera.uuid + '/recordings?startTime90k=' +
              selectedRange.startTime90k + '&endTime90k=' + selectedRange.endTime90k;
    if (split !== '') {
      url += '&split90k=' + split;
    }
    if (url === camera.recordingsUrl) {
      continue;  // nothing to do.
    }
    if (camera.recordingsReq !== null) {
      camera.recordingsReq.abort();
    }
    let tbody = $("#tab-" + camera.uuid);
    $(".r", tbody).remove();
    $(".loading", tbody).show();
    let r = req(url);
    camera.recordingsUrl = url;
    camera.recordingsRange = selectedRange;
    camera.recordingsReq = r;
    r.always(function() { camera.recordingsReq = null; });
    r.then(function(data, status, req) {
      // Sort recordings in descending order.
      data.recordings.sort(function(a, b) { return b.startId - a.startId; });
      camera.recordingsData = data;
      formatRecordings(camera);
    }).catch(function(data, status, err) {
      console.log(url, ' load failed: ', status, ': ', err);
    });
  }
}

// Run initially and when changing camera filter.
function setupCalendar() {
  let merged = {};
  for (const camera of cameras) {
    if (!camera.enabled) {
      continue;
    }
    for (const dateStr in camera.days) {
      merged[dateStr] = true;
    }
  }
  let minDateStr = '9999-99-99';
  let maxDateStr = '0000-00-00';
  for (const dateStr in merged) {
    if (dateStr > maxDateStr) {
      maxDateStr = dateStr;
    }
    if (dateStr < minDateStr) {
      minDateStr = dateStr;
    }
  }
  let from = $("#start-date");
  let to = $("#end-date");
  let beforeShowDay = function(date) {
    let dateStr = date.toISOString().substr(0, 10);
    return [dateStr in merged, "", ""];
  }
  if ($("#end-date-same").prop("checked")) {
    from.datepicker("option", {
      dateFormat: $.datepicker.ISO_8601,
      minDate: minDateStr,
      maxDate: maxDateStr,
      onSelect: function(dateStr, picker) {
        reselectDateRange(dateStr, dateStr);
      },
      beforeShowDay: beforeShowDay,
      disabled: false,
    });
    to.datepicker("destroy");
    to.datepicker({disabled: true});
  } else {
    from.datepicker("option", {
      dateFormat: $.datepicker.ISO_8601,
      minDate: minDateStr,
      onSelect: function(dateStr, picker) {
        to.datepicker("option", "minDate", from.datepicker("getDate").toISOString().substr(0, 10));
        reselectDateRange(dateStr, to.datepicker("getDate").toISOString().substr(0, 10));
      },
      beforeShowDay: beforeShowDay,
      disabled: false,
    });
    to.datepicker("option", {
      dateFormat: $.datepicker.ISO_8601,
      minDate: from.datepicker("getDate"),
      maxDate: maxDateStr,
      onSelect: function(dateStr, picker) {
        from.datepicker("option", "maxDate", to.datepicker("getDate").toISOString().substr(0, 10));
        reselectDateRange(from.datepicker("getDate").toISOString().substr(0, 10), dateStr);
      },
      beforeShowDay: beforeShowDay,
      disabled: false,
    });
    to.datepicker("setDate", from.datepicker("getDate"));
    from.datepicker("option", {maxDate: to.datepicker("getDate")});
  }
  const date = from.datepicker("getDate");
  if (date !== null) {
    const dateStr = date.toISOString().substr(0, 10);
    reselectDateRange(dateStr, dateStr);
  }
};

function onCameraChange(event, camera) {
  camera.enabled = event.target.checked;
  if (camera.enabled) {
    $("#tab-" + camera.uuid).show();
  } else {
    $("#tab-" + camera.uuid).hide();
  }
  console.log('Camera ', camera.shortName, camera.enabled ? 'enabled' : 'disabled');
  setupCalendar();
}

// Parses the given date and time string into a valid time90k or null.
function parseDateTime(dateStr, timeStr, isEnd) {
  // Match HH:mm[:ss[:FFFFF]][+-HH:mm]
  // Group 1 is the hour and minute (HH:mm).
  // Group 2 is the seconds (:ss), if any.
  // Group 3 is the fraction (FFFFF), if any.
  // Group 4 is the zone (+-HH:mm), if any.
  const timeRe =
      /^([0-9]{1,2}:[0-9]{2})(?:(:[0-9]{2})(?::([0-9]{5}))?)?([+-][0-9]{1,2}:?(?:[0-9]{2})?)?$/;

  if (timeStr === '') {
    const m = moment.tz(dateStr, zone);
    if (isEnd) {
      m.add({days: 1});
    }
    return m.valueOf() * 90;
  }

  const match = timeRe.exec(timeStr);
  if (match === null) {
    return null;
  }

  const orBlank = function(s) { return s === undefined ? "" : s; };
  const datetimeStr = dateStr + 'T' + match[1] + orBlank(match[2]) + orBlank(match[4]);
  const m = moment.tz(datetimeStr, zone);
  if (!m.isValid()) {
    return null;
  }

  const frac = match[3] === undefined ? 0 : parseInt(match[3], 10);
  return m.valueOf() * 90 + frac;
}

function onTimeChange(e, isEnd) {
  let parsed = parseDateTime(isEnd ? selectedRange.endDateStr : selectedRange.startDateStr,
                             e.target.value, isEnd);
  if (parsed == null) {
    console.log('bad time change');
    $(e.target).addClass('ui-state-error');
    return;
  }
  $(e.target).removeClass('ui-state-error');
  console.log(isEnd ? "end" : "start", ' time change to: ', parsed, ' (', formatTime(parsed), ')');
  if (isEnd) {
    selectedRange.endTimeStr = e.target.value;
    selectedRange.endTime90k = parsed;
  } else {
    selectedRange.startTimeStr = e.target.value;
    selectedRange.startTime90k = parsed;
  }
  fetch();
}

function onReceivedCameras(data) {
  let fieldset = $("#cameras");
  if (data.cameras.length === 0) {
    return;
  }
  var reqs = [];
  let videos = $("#videos");
  for (let camera of data.cameras) {
    const id = "cam-" + camera.uuid;
    let checkBox = $('<input type="checkbox" checked>').attr("name", id).attr("id", id);
    checkBox.change(function(event) { onCameraChange(event, camera); });
    fieldset.append(checkBox,
                    $("<label/>").attr("for", id).text(camera.shortName),
                    $("<br/>"));
    let tab = $("<tbody>").attr("id", "tab-" + camera.uuid);
    tab.append(
        $('<tr class="name">').append($('<th colspan=6/>').text(camera.shortName)),
        $('<tr class="hdr"><th>start</th><th>end</th><th>resolution</th><th>fps</th><th>size</th><th>bitrate</th></tr>'),
        $('<tr class="loading"><td colspan=6>loading...</td></tr>'));
    videos.append(tab);
    camera.enabled = true;
    camera.recordingsUrl = null;
    camera.recordingsRange = null;
    camera.recordingsData = null;
    camera.recordingsReq = null;
  }
  $("#end-date-same").change(function(e) { setupCalendar(); });
  $("#end-date-other").change(function(e) { setupCalendar(); });
  $("#start-date").datepicker({disabled: true});
  $("#end-date").datepicker({disabled: true});
  $("#start-time").change(function(e) { onTimeChange(e, false); });
  $("#end-time").change(function(e) { onTimeChange(e, true); });
  $("#split").change(function(e) {
    if (selectedRange.startTime90k !== null) {
      fetch();
    }
  });
  $("#trim").change(function(e) {
    // Changing the trim doesn't need to refetch data, but it does need to
    // reformat the tables.
    let newTrim = e.target.checked;
    for (camera of cameras) {
      if (camera.recordingsData !== null) {
        formatRecordings(camera);
      }
    }
  });
  zone = data.timeZoneName;
  cameras = data.cameras;
  console.log('Loaded cameras.');
  setupCalendar();
}

$(function() {
  $(document).tooltip();
  req(apiUrl + '?days=true').then(function(data, status, req) {
    onReceivedCameras(data);
  }).catch(function(data, status, err) {
    console.log('cameras load failed: ', status, err);
  });
});
