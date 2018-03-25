// vim: set et sw=2:

// TODO: test abort.
// TODO: add error bar on fetch failure.
// TODO: style: no globals? string literals? line length? fn comments?
// TODO: live updating.

import './favicon.ico';

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

const allStreamTypes = ['main', 'sub'];

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

// Cameras is a dictionary as retrieved from apiUrl + some extra props within
// the streams dicts:
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
 * Format timestamp using a format string.
 *
 * The timestamp to be formatted is expected to be in units of 90,000 to a
 * second (90k format).
 * 
 * The format string should comply with what is accepted by moment.format,
 * with one addition. A format pattern of FFFFF (5 Fs) can be used. This
 * format pattern will be replaced with the fractional second part of the
 * timestamp, still in 90k units. Thus if the timestamp was 89900 (which is
 * almost a full second; 0.99888 seconds decimal), the output would be
 * 89900, and NOT 0.99888. Only a pattern of five Fs is recognized and it
 * will produce exactly a five position output! You cannot vary the number
 * of Fs to produce less.
 *
 * The default format string was chosen to produce results identical to
 * a previous version of this code that was hard-coded to produce that output.
 * 
 * @param  {Number} ts90k  Timestamp in 90k units
 * @param  {String} format moment.format plus FFFFF pattern supported
 * @return {String}        Formatted timestamp
 */
function formatTime(ts90k, format = 'YYYY-MM-DDTHH:mm:ss:FFFFFZ') {
  const ms = ts90k / 90.0;
  const fracFmt = 'FFFFF';
  let fracLoc = format.indexOf(fracFmt);
  if (fracLoc != -1) {
    const frac = ts90k % 90000;
    format = format.substr(0, fracLoc) + String(100000 + frac).substr(1) +
             format.substr(fracLoc + fracFmt.length);
  }
  return moment.tz(ms, zone).format(format);
}

function onSelectVideo(camera, streamType, range, recording) {
  let url = apiUrl + 'cameras/' + camera.uuid + '/' + streamType + '/view.mp4?s=' + recording.startId;
  if (recording.endId !== undefined) {
    url += '-' + recording.endId;
  }
  if (recording.firstUncommitted !== undefined) {
    url += '@' + recording.openId;  // disambiguate.
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
  } else if (recording.growing !== undefined) {
    // View just the portion described here.
    rel += recording.endTime90k - recording.startTime90k;
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
      title: camera.shortName + " " + streamType + ", " + formattedStart + " to " + formattedEnd,
      width: recording.videoSampleEntryWidth / 4,
      close: () => {
        const videoDOMElement = video[0];
        videoDOMElement.pause();
        videoDOMElement.src = ''; // Remove current source to stop loading
        dialog.remove();
      },
  });
  video.attr("src", url);
}

function formatRecordings(camera, streamType) {
  let tbody = $("#tab-" + camera.uuid + "-" + streamType);
  $(".loading", tbody).hide();
  $(".r", tbody).remove();
  const frameRateFmt = new Intl.NumberFormat([], {maximumFractionDigits: 0});
  const sizeFmt = new Intl.NumberFormat([], {maximumFractionDigits: 1});
  const trim = $("#trim").prop("checked");
  const stream = camera.streams[streamType];
  for (const recording of stream.recordingsData.recordings) {
    const duration = (recording.endTime90k - recording.startTime90k) / 90000;
    let row = $('<tr class="r"/>');
    const startTime90k = trim && recording.startTime90k < stream.recordingsRange.startTime90k
        ? stream.recordingsRange.startTime90k : recording.startTime90k;
    const endTime90k = trim && recording.endTime90k > stream.recordingsRange.endTime90k
        ? stream.recordingsRange.endTime90k : recording.endTime90k;
    let formattedStart = formatTime(startTime90k);
    let formattedEnd = formatTime(endTime90k);
    const singleDateStr = stream.recordingsRange.singleDateStr;
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
    row.on("click", function() { onSelectVideo(camera, streamType, stream.recordingsRange, recording); });
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
    for (const streamType in camera.streams) {
      let stream = camera.streams[streamType];
      let url = apiUrl + 'cameras/' + camera.uuid + '/' + streamType + '/recordings?startTime90k=' +
                selectedRange.startTime90k + '&endTime90k=' + selectedRange.endTime90k;
      if (split !== '') {
        url += '&split90k=' + split;
      }
      if (url === stream.recordingsUrl) {
        continue;  // nothing to do.
      }
      console.log('url: ', url);
      if (stream.recordingsReq !== null) {
        stream.recordingsReq.abort();
      }
      let tbody = $("#tab-" + camera.uuid + "-" + streamType);
      $(".r", tbody).remove();
      $(".loading", tbody).show();
      let r = req(url);
      stream.recordingsUrl = url;
      stream.recordingsRange = selectedRange;
      stream.recordingsReq = r;
      r.always(function() { stream.recordingsReq = null; });
      r.then(function(data, status, req) {
        // Sort recordings in descending order.
        data.recordings.sort(function(a, b) { return b.startId - a.startId; });
        stream.recordingsData = data;
        formatRecordings(camera, streamType);
      }).catch(function(data, status, err) {
        console.log(url, ' load failed: ', status, ': ', err);
      });
    }
  }
}

// Run initially and when changing camera filter.
function setupCalendar() {
  let merged = {};
  for (const camera of cameras) {
    for (const streamType in camera.streams) {
      const stream = camera.streams[streamType];
      if (!stream.enabled) {
        continue;
      }
      for (const dateStr in stream.days) {
        merged[dateStr] = true;
      }
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

function onStreamChange(event, camera, streamType) {
  let stream = camera.streams[streamType];
  stream.enabled = event.target.checked;
  let id = "#tab-" + camera.uuid + "-" + streamType;
  if (stream.enabled) {
    $(id).show();
  } else {
    $(id).hide();
  }
  console.log(camera.shortName + "/" + streamType, stream.enabled ? 'enabled' : 'disabled');
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
  let camtable = $("#cameras");
  if (data.cameras.length === 0) {
    return;
  }

  // Add a header row.
  let hdr = $('<tr/>').append($('<th/>'));
  for (const streamType of allStreamTypes) {
    hdr.append($('<th/>').text(streamType));
  }
  camtable.append(hdr);

  var reqs = [];
  let videos = $("#videos");
  for (let camera of data.cameras) {
    let row = $('<tr/>').append($('<td>').text(camera.shortName));
    let anyCheckedForCam = false;
    for (const streamType of allStreamTypes) {
      let stream = camera.streams[streamType];
      if (stream === undefined) {
        row.append('<td/>');
        continue;
      }
      const id = "cam-" + camera.uuid + "-" + streamType;
      let checkBox = $('<input type="checkbox">').attr("name", id).attr("id", id);
      checkBox.change(function(event) { onStreamChange(event, camera, streamType); });
      row.append($("<td/>").append(checkBox));
      let tab = $("<tbody>").attr("id", "tab-" + camera.uuid + "-" + streamType);
      tab.append(
          $('<tr class="name">').append($('<th colspan=6/>').text(camera.shortName + " " + streamType)),
          $('<tr class="hdr"><th>start</th><th>end</th><th>resolution</th><th>fps</th><th>size</th><th>bitrate</th></tr>'),
          $('<tr class="loading"><td colspan=6>loading...</td></tr>'));
      videos.append(tab);
      stream.recordingsUrl = null;
      stream.recordingsRange = null;
      stream.recordingsData = null;
      stream.recordingsReq = null;
      stream.enabled = false;
      if (!anyCheckedForCam) {
        checkBox.attr("checked", "checked");
        anyCheckedForCam = true;
        stream.enabled = true;
      } else {
        tab.hide();
      }
    }
    camtable.append(row);
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
      for (streamType in camera.streams) {
        const stream = camera.streams[streamType];
        if (stream.recordingsData !== null) {
          formatRecordings(camera, streamType);
        }
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
