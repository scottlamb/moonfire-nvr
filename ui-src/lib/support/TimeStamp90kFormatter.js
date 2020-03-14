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

import TimeFormatter from './TimeFormatter';


export const internalTimeFormat = 'YYYY-MM-DDTHH:mm:ss:FFFFFZ';

/**
 * Specialized class similar to TimeFormatter but forcing a specific time format
 * for internal usage purposes.
 */
export default class TimeStamp90kFormatter {
  /**
   * Construct from just a timezone specification.
   *
   * @param  {String} tz Timezone
   */
  constructor(tz) {
    this.formatter_ = new TimeFormatter(internalTimeFormat, tz);
  }

  /**
   * Format a timestamp in 90k units using internal format.
   *
   * @param {Number} ts90k timestamp in 90,000ths of a second resolution
   * @return {String}        Formatted timestamp
   */
  formatTimeStamp90k(ts90k) {
    return this.formatter_.formatTimeStamp90k(ts90k);
  }

  /**
   * Given two timestamp return formatted versions of both, where the second
   * one may have been shortened if it falls on the same date as the first one.
   *
   * @param  {Number} ts1 First timestamp in 90k units
   * @param  {Number} ts2 Secodn timestamp in 90k units
   * @return {Array}     Array with two elements: [ ts1Formatted, ts2Formatted ]
   */
  formatSameDayShortened(ts1, ts2) {
    let ts1Formatted = this.formatTimeStamp90k(ts1);
    let ts2Formatted = this.formatTimeStamp90k(ts2);
    const timePos = this.formatter_.formatStr.indexOf('T');
    if (timePos != -1) {
      const datePortion = ts1Formatted.substr(0, timePos);
      ts1Formatted = datePortion + ' ' + ts1Formatted.substr(timePos + 1);
      if (ts2Formatted.startsWith(datePortion)) {
        ts2Formatted = ts2Formatted.substr(timePos + 1);
      }
    }
    return [ts1Formatted, ts2Formatted];
  }
}
