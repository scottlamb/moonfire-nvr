// vim: set et sw=2:
//

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
}
