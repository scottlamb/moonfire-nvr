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
   * The date and time strings must be compatible with the partial ISO-8601
   * formats for each, or acceptable to the standard Date object.
   *
   * If only a date is specified and dateOnlyThenEndOfDay is false, the 00:00
   * timestamp for that day is returned. If dateOnlyThenEndOfDay is true, the
   * 00:00 of the very next day is returned.
   *
   * @param  {String}  dateStr String representing date
   * @param  {String}  timeStr String representing time
   * @param  {Boolean} dateOnlyThenEndOfDay   If only a date was specified and
   *                                          this is true, then return time
   *                                          for the end of day
   * @return {Number}          Timestamp in 90k units, or null if parsing failed
   */
  parseDateTime90k(dateStr, timeStr, dateOnlyThenEndOfDay) {
    // If just date, no special handling needed
    if (!timeStr) {
      const m = moment.tz(dateStr, self._tz);
      if (dateOnlyThenEndOfDay) {
        m.add({days: 1});
      }
      return m.valueOf() * 90;
    }

    const [match, hhmm, ss, fffff, tz] = timeRe.exec(timeStr) || [];
    if (!match) {
      return null;
    }

    const orBlank = (s) => s || '';
    const datetimeStr = dateStr + 'T' + hhmm + orBlank(ss) + orBlank(tz);
    const m = moment.tz(datetimeStr, self._tz);
    if (!m.isValid()) {
      return null;
    }

    const frac = fffff === undefined ? 0 : parseInt(fffff, 10);
    return m.valueOf() * 90 + frac;
  }
}
