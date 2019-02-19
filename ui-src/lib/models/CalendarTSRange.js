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

import Time90kParser from '../support/Time90kParser';
import TimeStamp90kFormatter from '../support/TimeStamp90kFormatter';
import Range90k from './Range90k';

/**
 * Class representing a calendar timestamp range based on 90k units.
 *
 * A calendar timestamp differs from a Range90k in that a date string
 * is involved on each end as well.
 *
 * The range has a start and end property (via getters) and each has three
 * contained properties:
 * - dateStr: string for date in ISO8601 format
 * - timeStr: string for time in ISO8601 format
 * - ts90k: Number for the timestamp in 90k units
 */
export default class CalendarTSRange {
  /**
   * Construct a range with a given timezone for display purposes.
   *
   * @param  {String} timeZone Desired timezone, e.g. 'America/Los_Angeles'
   */
  constructor(timeZone) {
    this._start = {dateStr: null, timeStr: '', ts90k: null};
    this._end = {dateStr: null, timeStr: '', ts90k: null};
    // Don't need to keep timezone, but need parser and formatter
    this._timeFormatter = new TimeStamp90kFormatter(timeZone);
    this._timeParser = new Time90kParser(timeZone);
  }

  /**
   * Determine if a valid start date string is present.
   *
   * @return {Boolean}
   */
  hasStart() {
    return this.start.dateStr !== null;
  }

  /**
   * Determine if a valid end date string is present.
   *
   * @return {Boolean}
   */
  hasEnd() {
    return this.end.dateStr !== null;
  }

  /**
   * Determine if a valid start and end date string is present.
   *
   * @return {Boolean}
   */
  hasRange() {
    return this.hasStart() && this.hasEnd();
  }

  /**
   * Return the range's start component.
   *
   * @return {object} Object containing dateStr, timeStr, and ts90k components
   */
  get start() {
    return this._start;
  }

  /**
   * Return the range's end component.
   *
   * @return {object} Object containing dateStr, timeStr, and ts90k components
   */
  get end() {
    return this._end;
  }

  /**
   * Return the range's start component's ts90k property
   *
   * @return {object} timestamp in 90k units
   */
  get startTime90k() {
    return this.start.ts90k;
  }

  /**
   * Return the range's end component's ts90k property
   *
   * @return {object} timestamp in 90k units
   */
  get endTime90k() {
    return this.end.ts90k;
  }

  /**
   * Determine if the range has a defined start timestamp in 90k units.
   *
   * @return {Boolean}
   */
  get hasStartTime() {
    return this.startTime90k !== null;
  }

  /**
   * Return the calendar range in terms of a range over 90k timestamps.
   *
   * @return {Range90k} Range object or null if don't have start and end
   */
  range90k() {
    return this.hasRange()
      ? new Range90k(this.startTime90k, this.endTime90k)
      : null;
  }

  /**
   * Internal function to update either start or end type range component.
   *
   * Strings are parsed to check if they are valid. Update only takes place
   * if they are. Parsing is in accordance with the installed Time90kParser
   * which means:
   * - HH:MM:ss:FFFFFZ format, where each component may be empty to indicate 0
   * - YYYY-MM-DD format for the date
   *
   * NOTE: This function potentially modifies the content of the range
   * argument. This is on purpose and should reflect the new range values
   * upon successful parsing!
   *
   * @param {object} range   A range component
   * @param {String} dateStr Date string
   * @param {String} timeStr Time string
   * @param {Boolean} dateOnlyThenEndOfDay  True if one should be added to date
   *                                        which is only meaningful if there
   *                                        is no time specified here, and also
   *                                        not present in the range.
   * @return {Number} New timestamp if succesfully parsed, null otherwise
   */
  _setRangeTime(range, dateStr, timeStr, dateOnlyThenEndOfDay) {
    const newTs90k = this._timeParser.parseDateTime90k(
      dateStr,
      timeStr,
      dateOnlyThenEndOfDay
    );
    if (newTs90k !== null) {
      range.dateStr = dateStr;
      range.timeStr = timeStr;
      range.ts90k = newTs90k;
      return newTs90k;
    }
    return null;
  }

  /**
   * Set start component of range from date and time strings.
   *
   * Uses _setRangeTime with appropriate dateOnlyThenEndOfDay value.
   *
   * @param {String} dateStr Date string
   * @return {Number} New timestamp if succesfully parsed, null otherwise
   */
  setStartDate(dateStr) {
    return this._setRangeTime(this._start, dateStr, this._start.timeStr, false);
  }

  /**
   * Set time of start component of range time string.
   *
   * Uses _setRangeTime with appropriate dateOnlyThenEndOfDay value.
   *
   * @param {String} timeStr Time string
   * @return {Number} New timestamp if succesfully parsed, null otherwise
   */
  setStartTime(timeStr) {
    return this._setRangeTime(this._start, this._start.dateStr, timeStr, false);
  }

  /**
   * Set end component of range from date and time strings.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} dateStr Date string
   * @return {Number} New timestamp if succesfully parsed, null otherwise
   */
  setEndDate(dateStr) {
    return this._setRangeTime(this._end, dateStr, this._end.timeStr, true);
  }

  /**
   * Set time of end component of range time string.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} timeStr Time string
   * @return {Number} New timestamp if succesfully parsed, null otherwise
   */
  setEndTime(timeStr) {
    return this._setRangeTime(this._end, this._end.dateStr, timeStr, true);
  }

  /**
   * Format a timestamp in 90k units in the manner consistent with
   * what the parser of this module expects.
   *
   * @param  {Number} ts90k Timestamp in 90k units
   * @return {String}       Formatted string
   */
  formatTimeStamp90k(ts90k) {
    return this._timeFormatter.formatTimeStamp90k(ts90k);
  }
}
