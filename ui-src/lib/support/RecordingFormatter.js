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

import TimeFormatter from './TimeFormatter';

/**
 * Formatter for framerates
 * @type {Intl} Formatter
 */
const frameRateFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 0,
});

/**
 * Formatter for sizes
 * @type {Intl} Formatter
 */
const sizeFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 1,
});

/**
 * Class encapsulating formatting of recording time ranges.
 */
export default class RecordingFormatter {
  /**
   * Construct with desired time format and timezone.
   *
   * @param  {String} formatStr Time format string
   * @param  {String} tz        Timezone
   */
  constructor(formatStr, tz) {
    this._timeFormatter = new TimeFormatter(formatStr, tz);
    this._singleDateStr = null;
  }

  /**
   * Change time format string, preserving timezone.
   *
   * @param  {String} formatStr Time format string
   */
  set timeFormat(formatStr) {
    this._timeFormatter = new TimeFormatter(formatStr, this._timeFormatter.tz);
  }

  /**
   * [format description]
   * @param  {Recording} recording    Recording to be formatted
   * @param  {Range90k} trimRange     Optional time range for trimming the
   *                                  recording's interval
   * @return {Object}                 Map, keyed by _columnOrder element
   */
  format(recording, trimRange = null) {
    const duration = recording.duration;
    const trimmedRange = recording.range90k(trimRange);
    return {
      start: this._timeFormatter.formatTimeStamp90k(trimmedRange.startTime90k),
      end: this._timeFormatter.formatTimeStamp90k(trimmedRange.endTime90k),
      resolution:
        recording.videoSampleEntryWidth +
        'x' +
        recording.videoSampleEntryHeight,
      frameRate: frameRateFmt.format(recording.frameCount / duration),
      size: sizeFmt.format(recording.sampleFileBytes / 1048576) + ' MB',
      rate:
        sizeFmt.format(recording.sampleFileBytes / duration * 0.000008) +
        ' Mbps',
    };
  }
}
