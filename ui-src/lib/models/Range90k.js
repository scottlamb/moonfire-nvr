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

import Range from './Range';

/**
 * WeakMap that keeps our private data.
 *
 * @type {WeakMap}
 */
let _range = new WeakMap();

/**
 * Subclass of Range to represent ranges over timestamps in 90k format.
 *
 * This mostly means added some getters with names that make more sense.
 */
export default class Range90k {
  /**
   * Create a range.
   *
   * @param  {Number} low  Low value (inclusive) in range.
   * @param  {Number} high High value (inclusive) in range.
   */
  constructor(low, high) {
    _range.set(this, new Range(low, high));
  }

  /**
   * Return the range's start time.
   *
   * @return {Number} Number in 90k units
   */
  get startTime90k() {
    return _range.get(this).low;
  }

  /**
   * Return the range's end time.
   *
   * @return {Number} Number in 90k units
   */
  get endTime90k() {
    return _range.get(this).high;
  }

  /**
   * Return the range's duration.
   *
   * @return {Number} Number in 90k units
   */
  get duration90k() {
    return _range.get(this).size;
  }

  /**
   * Create a new range by trimming the current range against
   * another.
   *
   * The returned range will lie completely within the provided range.
   *
   * @param  {Range90k} against Range the be used for limits
   * @return {Range90k}         The trimmed range (always a new object)
   */
  trimmed(against) {
    return new Range90k(
      Math.max(this.startTime90k, against.startTime90k),
      Math.min(this.endTime90k, against.endTime90k)
    );
  }

  /**
   * Return a copy of this range.
   *
   * @return {Range90k} A copy of this range object.
   */
  clone() {
    return new Range90k(this.startTime90k, this.endTime90k);
  }
}
