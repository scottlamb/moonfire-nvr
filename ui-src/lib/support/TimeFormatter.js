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

import moment from 'moment-timezone';

export const internalTimeFormat = 'YYYY-MM-DDTHH:mm:ss:FFFFFZ';
export const defaultTimeFormat = 'YYYY-MM-DD HH:mm:ss';

/**
 * Class for formatting timestamps.
 *
 * There are methods for formatting timestamp in three different unit systems:
 * - 90k: The units are multiples of 1/90,000th of a second
 * - Sec: The units are multiples of seconds
 * - Ms: The units are multiples of milliseconds
 *
 * The object is initialized with a format string and a timezone. The timezone
 * is necessary to format times in that timezone.
 *
 * The format string is based on those accepted by moment.js with one addition
 * detailed in formatTimeStamp90k.
 */
export default class TimeFormatter {
  /**
   * Construct with specific format string and timezone.
   *
   * @param  {String} formatStr Format specification string
   * @param  {String} tz        Timezone, e.g. "America/Los_Angeles"
   */
  constructor(formatStr, tz) {
    this._formatStr = formatStr || defaultTimeFormat;
    this._tz = tz;
  }

  /**
   * Get current format string
   *
   * @return {String} Format specification string
   */
  get formatStr() {
    return this._formatStr;
  }

  /**
   * Get current timezone
   *
   * @return {String} Timezone
   */
  get tz() {
    return this._tz;
  }

  /**
   * Produces a human-readable timestamp in 90k units.
   *
   * The format is anything understood by moment's format function,
   * with the addition of one special format indicator consisting of
   * five successive Fs. If this pattern is used more than once,
   * only the first one will be handled. Subsequent ones will become
   * literal strings with five Fs.
   *
   * Using normal format codes, precision of up the three S (SSS) is
   * supported by moment to display decimal seconds. "moment" truncates
   * the value passed in to its constructor, effectively truncating
   * any fractional values in the timestamp. This function rounds
   * to compensate for that, except in the case of the FFFFF pattern,
   * where rounding is left out for historical reasons.
   *
   * FFFFF produces a string indicating how many 90k units are present
   * in the sub-second portion of the timestamp. Therefore this is *not*
   * a decimal fraction!
   *
   * @param {Number} ts90k timestamp in 90,000ths of a second resolution
   * @return {String}        Formatted timestamp
   */
  formatTimeStamp90k(ts90k) {
    let format = this._formatStr;
    const ms = ts90k / 90.0;
    const fracFmt = 'FFFFF';
    let fracLoc = format.indexOf(fracFmt);
    if (fracLoc != -1) {
      const frac = ts90k % 90000;
      format =
        format.substr(0, fracLoc) +
        String(100000 + frac).substr(1) +
        format.substr(fracLoc + fracFmt.length);
    }
    return moment.tz(ms, this._tz).format(format);
  }

  /**
   * Format timestamp expressed in mill-seconds.
   *
   * @param  {Number} ms     A timestamp in ms to be formatted
   * @return {String}        Formatted timestamp
   */
  formatTimeStampMs(ms) {
    // Convert to 90k value first
    return this.formatTimeStamp90k(ms * 90);
  }

  /**
   * Format timestamp expressed in mill-seconds.
   *
   * @param  {Number} s      A timestamp in s to be formatted
   * @return {String}        Formatted timestamp
   */
  formatTimeStampSec(s) {
    // Convert to 90k value first
    return this.formatTimeStamp90k(s * 90000);
  }
}

/**
 * Specialized class similar to TimeFormatter but forcing a specific time format
 * for internal usage purposes.
 */
export class TimeStamp90kFormatter {
  /**
   * Construct from just a timezone specification.
   *
   * @param  {String} tz Timezone
   */
  constructor(tz) {
    this._formatter = new TimeFormatter(internalTimeFormat, tz);
  }

  /**
   * Format a timestamp in 90k units using internal format.
   *
   * @param {Number} ts90k timestamp in 90,000ths of a second resolution
   * @return {String}        Formatted timestamp
   */
  formatTimeStamp90k(ts90k) {
    return this._formatter.formatTimeStamp90k(ts90k);
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
    let timePos = this._formatter.formatStr.indexOf('T');
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
