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
