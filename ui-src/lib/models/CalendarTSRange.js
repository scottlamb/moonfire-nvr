// vim: set et sw=2:
//

import Time90kParser from '../support/Time90kParser';
import { TimeStamp90kFormatter } from '../support/TimeFormatter';

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
    this._start = { dateStr: null, timeStr: '', ts90k: null };
    this._end = { dateStr: null, timeStr: '', ts90k: null };
    // Don't need to keep timezone, but need parser and formatter
    this._timeFormatter = new TimeStamp90kFormatter(timeZone);
    this._timeParser = new Time90kParser(timeZone);
  }

  hasStart() {
    return this.start.dateStr !== null;
  }
  hasEnd() {
    return this.end.dateStr !== null;
  }

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
   * @return {Boolean} True if it is defined
   */
  get hasStartTime() {
    return this.startTime90k !== null;
  }

  /**
   * Internal function to update either start or end type range component.
   *
   * Strings are parsed to check if they are valid. Update only takes place
   * if they are. Parsing is in accordance with the installed Time90kParser
   * which means:
   * - HH:MM:ss:FFFFFZ format, where each componet may be empty to indicate 0
   * - YYYY-MM-DD format for the date
   *
   * @param {object} range   A range component
   * @param {String} dateStr Date string, if null range's value is re-used
   * @param {String} timeStr Time string, if null range's value is re-used
   * @param {Boolean} addOne  True if one should be added to date (for end)
   * @return {void}
   */
  _setRangeTime(range, dateStr, timeStr, addOne) {
    const newTs90k = this._timeParser.parseDateTime90k(
      dateStr || range.dateStr,
      timeStr || range.timeStr,
      addOne
    );
    if (newTs90k !== null) {
      range.dateStr = dateStr;
      range.ts90k = newTs90k;
      return newTs90k;
    }
    return null;
  }

  /**
   * Set start component of range from date and time strings.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} dateStr Date string
   * @param {String} timeStr Time string
   * @return {void}
   */
  setStartDate(dateStr, timeStr = null) {
    return this._setRangeTime(this._start, dateStr, timeStr, false);
  }

  /**
   * Set time of start component of range time string.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} timeStr Time string
   * @return {void}
   */
  setStartTime(timeStr) {
    return this._setRangeTime(this._start, null, timeStr, false);
  }

  /**
   * Set end component of range from date and time strings.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} dateStr Date string
   * @param {String} timeStr Time string
   * @return {void}
   */
  setEndDate(dateStr, timeStr = null) {
    return this._setRangeTime(this._end, dateStr, timeStr, true);
  }

  /**
   * Set time of end component of range time string.
   *
   * Uses _setRangeTime with appropriate addOne value.
   *
   * @param {String} timeStr Time string
   * @return {void}
   */
  setEndTime(timeStr) {
    return this._setRangeTime(this._end, null, timeStr, true);
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
