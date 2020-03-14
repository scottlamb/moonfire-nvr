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

import Recording from '../models/Recording';

/**
 * Desired column order in recordings table.
 *
 * The column names must correspond to the propertu names in the JSON
 * representation of recordings.
 *
 * @todo This should be decoupled!
 *
 * @type {Array} Array of column names
 */
const _columnOrder = [
  'start',
  'end',
  'resolution',
  'frameRate',
  'size',
  'rate',
];

/**
 * Labels for columns.
 */
const _columnLabels = {
  start: 'Start',
  end: 'End',
  resolution: 'Resolution',
  frameRate: 'FPS',
  size: 'Storage',
  rate: 'BitRate',
};

/**
 * Class to encapsulate a view of a list of recordings from a single camera.
 */
export default class RecordingsView {
  /**
   * Construct display from camera data and use supplied formatter.
   *
   * @param  {Camera} camera camera object (immutable)
   * @param  {String} streamType "main" or "sub"
   * @param  {RecordingFormatter} recordingFormatter Desired formatter
   * @param  {Boolean} trimmed True if the display should include trimmed ranges
   * @param  {jQuery} parent Parent to which new DOM is attached, or null
   */
  constructor(camera, streamType, recordingFormatter, trimmed = false,
      parent = null) {
    this.cameraName_ = camera.shortName;
    this.cameraRange_ = camera.range90k;
    this.formatter_ = recordingFormatter;

    const id = `tab-${camera.uuid}-${streamType}`;
    this.element_ = this.createElement_(id, camera.shortName, streamType);
    this.trimmed_ = trimmed;
    this.recordings_ = null;
    this.recordingsRange_ = null;
    this.clickHandler_ = null;
    if (parent) {
      parent.append(this.element_);
    }
    this.timeoutId_ = null;
  }

  /**
   * Create DOM for the recording.
   *
   * @param {String} id DOM id for the main element
   * @param {String} cameraName Name of the corresponding camera
   * @param  {String} streamType "main" or "sub"
   * @return {jQuery} Partial DOM as jQuery object
   */
  createElement_(id, cameraName, streamType) {
    const tab = $('<tbody>').attr('id', id);
    tab.append(
        $('<tr class="name">').append($('<th colspan=6/>')
            .text(cameraName + ' ' + streamType)),
        $('<tr class="hdr">').append(
            $(
                _columnOrder
                    .map((name) => '<th>' + _columnLabels[name] + '</th>')
                    .join('')
            )
        ),
        $('</tr>'),
        $('<tr class="loading"><td colspan=6>loading...</td></tr>').hide()
    );
    return tab;
  }

  /**
   * Update display for new recording values.
   *
   * Each existing row is reformatted.
   *
   * @param  {Array} newRecordings
   * @param  {Boolean} trimmed  True if timestamps should be trimmed
   */
  updateRecordings_() {
    const trimRange = this.trimmed_ ? this.recordingsRange : null;
    const recordings = this.recordings_;
    this.element_.children('tr.r').each((rowIndex, row) => {
      const values = this.formatter_.format(recordings[rowIndex], trimRange);
      $(row)
          .children('td')
          .each((i, e) => $(e).text(values[_columnOrder[i]]));
    });
  }

  /**
   * Get the currently remembered recordings range for this view.
   *
   * This range corresponds to what was in the data time range selector UI
   * at the time the data for this view was selected. The value is remembered
   * purely for trimming purposes.
   *
   * @return {Range90k} Currently remembered range
   */
  get recordingsRange() {
    return this.recordingsRange_ ? this.recordingsRange_.clone() : null;
  }

  /**
   * Set the recordings range for this view.
   *
   * @param  {Range90k} range90k Range to remember
   */
  set recordingsRange(range90k) {
    this.recordingsRange_ = range90k ? range90k.clone() : null;
  }

  /**
   * Get whether time ranges in the recording list are being trimmed.
   *
   * @return {Boolean}
   */
  get trimmed() {
    return this.trimmed_;
  }

  /**
   * Set whether recording time ranges should be trimmed.
   *
   * @param  {Boolean} value True if trimming desired
   */
  set trimmed(value) {
    if (value != this.trimmed_) {
      this.trimmed_ = value;
      this.updateRecordings_();
    }
  }

  /**
   * Show or hide the display in the DOM.
   *
   * @param  {Boolean} show True for show, false for hide
   */
  set show(show) {
    const sel = this.element_;
    if (show) {
      sel.show();
    } else {
      sel.hide();
    }
  }

  /**
   * Set whether loading indicator should be shown or not.
   *
   * @param  {Boolean} show True if indicator should be showing
   */
  set showLoading(show) {
    const loading = $('tr.loading', this.element_);
    if (show) {
      loading.show();
    } else {
      if (this.timeoutId_) {
        clearTimeout(this.timeoutId_);
        this.timeoutId_ = null;
      }
      loading.hide();
    }
  }

  /**
   * Show the loading indicated after a delay, unless the timer has been
   * cleared already.
   *
   * @param  {Number} timeOutMs Delay (in ms) before indicator should appear
   */
  delayedShowLoading(timeOutMs) {
    this.timeoutId_ = setTimeout(() => (this.showLoading = true), timeOutMs);
  }

  /**
   * Set a new time format string.
   *
   * This string is passed on to the formatter and the recordings list
   * is updated (using the formatter).
   *
   * @param  {String} formatStr Formatting string
   */
  set timeFormat(formatStr) {
    // Change the formatter and update recordings (view)
    this.formatter_.timeFormat = formatStr;
    this.updateRecordings_();
  }

  /**
   * Set a handler to receive clicks on a recording.
   *
   * The handler will be called with one argument: a recording model.
   *
   * @param  {Function} h Handler to be called.
   */
  set onRecordingClicked(h) {
    this.clickHandler_ = h;
  }

  /**
   * Set the list of recordings from JSON data.
   *
   * The data is expected to be an array with recording objects.
   *
   * @param  {object} recordingsJSON JSON data (object)
   */
  set recordingsJSON(recordingsJSON) {
    this.showLoading = false;
    // Store as model objects
    this.recordings_ = recordingsJSON.recordings.map(function(r) {
      const vse = recordingsJSON.videoSampleEntries[r.videoSampleEntryId];
      return new Recording(r, vse);
    });

    const tbody = this.element_;
    // Remove existing rows, replace with new ones
    $('tr.r', tbody).remove();
    this.recordings_.forEach((r) => {
      const row = $('<tr class="r" />');
      row.append(_columnOrder.map(() => $('<td/>')));
      row.on('click', () => {
        console.log('Video clicked');
        if (this.clickHandler_ !== null) {
          console.log('Video clicked handler call');
          this.clickHandler_(r);
        }
      });
      tbody.append(row);
    });
    // Cause formatting and date to be put in the rows
    this.updateRecordings_();
  }
}
