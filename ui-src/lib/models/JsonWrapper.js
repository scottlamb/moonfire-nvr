// vim: set et sw=2:
//

/**
 * WeakMap that keeps our private data.
 *
 * @type {WeakMap}
 */
let _json = new WeakMap();

/**
 * Class to encapsulate recording JSON data.
 * *
 * The JSON is kept internally, but in a manner that does not allow direct
 * access. If access is needed, use the "json()" method. Sub-classes for
 * specific models shoudl provide the necessary getters instead.
 */
export default class JsonWrapper {
  /**
   * Accept JSON data to be encapsulated
   *
   * @param  {object} jsonData JSON data
   */
  constructor(jsonData) {
    _json.set(this, jsonData);
  }

  /**
   * Get associated JSON object.
   *
   * Use of this should be avoided. Use functions to access the
   * data instead.
   *
   * @return {object} The JSON object.
   */
  get json() {
    return _json.get(this);
  }

  /**
   * @override
   * @return {String} String version
   */
  toString() {
    if (process.env.NODE_ENV === 'development') {
      return this.json.toString();
    } else {
      return super.toString();
    }
  }
}
