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

/**
 * Class to represent ranges of values.
 *
 * The range has a "low", and "high" value property and is inclusive.
 * The "size" property returns the difference between high and low.
 */
export default class Range {
  /**
   * Create a range.
   *
   * @param  {Number} low  Low value (inclusive) in range.
   * @param  {Number} high High value (inclusive) in range.
   */
  constructor(low, high) {
    if (high < low) {
      console.log('Warning range swap: ' + low + ' - ' + high);
      [low, high] = [high, low];
    }
    this.low = low;
    this.high = high;
  }

  /**
   * Size of the range.
   *
   * @return {Number} high - low
   */
  get size() {
    return this.high - this.low;
  }

  /**
   * Determine if value is inside the range.
   *
   * @param  {Number}  value Value to test
   * @return {Boolean}       True if value inside the range
   */
  isInRange(value) {
    return value >= this.low && value <= this.high;
  }
}
