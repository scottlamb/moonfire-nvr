// vim: set et sw=2:
//

/**
 * Class to help with URL construction.
 */
export default class URLBuilder {
  /**
   * Construct builder with a base url.
   *
   * It is possible to indicate the we only want to extract relative
   * urls. In that case, pass a dummy scheme and host.
   *
   * @param  {String}  base     Base url, including scheme and host
   * @param  {Boolean} relative True if relative urls desired
   */
  constructor(base, relative = true) {
    this._baseUrl = base;
    this._relative = relative;
  }

  /**
   * Append query parameters from a map to a URL.
   *
   * This is cumulative, so if you call this multiple times on the same URL
   * the resulting URL will have the combined query parameters and values.
   *
   * @param {URL} url   URL to add query parameters to
   * @param {Object} query Object with parameter name/value pairs
   * @return {URL} URL where query params have been added
   */
  _addQuery(url, query = {}) {
    Object.entries(query).forEach(([k, v]) => url.searchParams.set(k, v));
    return url;
  }

  /**
   * Construct a String url based on an initial path and an optional set
   * of query parameters.
   *
   * The url will be constructed based on the base url, with path appended.
   *
   * @param  {String} path  Path to be added to base url
   * @param  {Object} query Object with query parameters
   * @return {String}       Formatted url, relative if so configured
   */
  makeUrl(path, query = {}) {
    const url = new URL(path || '', this._baseUrl);
    this._addQuery(url, query);
    return this._relative ? url.pathname + url.search : url.href;
  }
}
