// vim: set et sw=2:
//

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
