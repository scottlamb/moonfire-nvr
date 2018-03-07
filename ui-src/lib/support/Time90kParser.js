// vim: set et sw=2:
//

import moment from 'moment-timezone';

/**
 * Regular expression for parsing time format from timestamps.
 *
 * These regex captures groups:
 * 0: whole match or null if none
 * 1: HH:MM portion, or undefined
 * 2: :ss portion, or undefined
 * 3: FFFFF portion, or undefined
 * 4: [+-]hh[:mm] portion, or undefined
 *
 * @type {RegExp}
 */
const timeRe = new RegExp(
  [
    '^', // Start
    '([0-9]{1,2}:[0-9]{2})', // Capture HH:MM
    '(?:(:[0-9]{2})(?::([0-9]{5}))?)?', // Capture [:ss][:FFFFF]
    '([+-][0-9]{1,2}:?(?:[0-9]{2})?)?', // Capture [+-][zone]
    '$', // End
  ].join('')
);

/**
 * Class to parse time strings that possibly contain fractional
 * seconds in 90k units into a Number representation.
 *
 * The general format:
 * Expected timestamps are in this format:
 *   HH:MM[:ss][:FFFFF][[+-]hh[:mm]]
 * where
 * HH = hours in one or two digits
 * MM = minutes in one or two digits
 * ss = seconds in one or two digits
 * FFFFF = fractional seconds in 90k units in exactly 5 digits
 * hh = hours of timezone offset in one or two digits
 * mm = minutes of timezone offset in one or two digits
 *
 */
export default class Time90kParser {
  /**
   * Construct with specific timezone.
   *
   * @param  {String} tz Timezone
   */
  constructor(tz) {
    self._tz = tz;
  }

  /**
   * Set (another) timezone.
   *
   * @param  {String} tz Timezone
   */
  set tz(tz) {
    self._tz = tz;
  }

  /**
   * Parses the given date and time string into a valid time90k or null.
   *
   * @param  {[type]}  dateStr [description]
   * @param  {[type]}  timeStr [description]
   * @param  {Boolean} isEnd   [description]
   * @return {[type]}          [description]
   */
  parseDateTime90k(dateStr, timeStr, isEnd) {
    // If just date, no special handling needed
    if (!timeStr) {
      const m = moment.tz(dateStr, self._tz);
      if (isEnd) {
        m.add({days: 1});
      }
      return m.valueOf() * 90;
    }

    const [match, hhmm, ss, fffff, tz] = timeRe.exec(timeStr) || [];
    if (!match) {
      return null;
    }

    const orBlank = (s) => (s === undefined ? '' : s);
    const datetimeStr = dateStr + 'T' + hhmm + orBlank(ss) + orBlank(tz);
    const m = moment.tz(datetimeStr, self._tz);
    if (!m.isValid()) {
      return null;
    }

    const frac = fffff === undefined ? 0 : parseInt(fffff, 10);
    return m.valueOf() * 90 + frac;
  }
}
