// vim: set et sw=2 ts=2:
//
// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2018 The Moonfire NVR Authors
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

import $ from 'jquery';

import 'jquery-ui/themes/base/core.css';
import 'jquery-ui/themes/base/datepicker.css';
import 'jquery-ui/themes/base/theme.css';
import 'jquery-ui/ui/widgets/datepicker';

/**
 * Class to encapsulate datepicker UI widget from jQuery.
 */
export default class DatePickerView {
  /**
   * Get the singleton datepicker instance.
   *
   * This is useful for accessing implementation constants, such as
   * date formats etc.
   *
   * @return {jQuery.datepicker} JQuery datepicker instance
   */
  static get datepicker() {
    return $.datepicker;
  }

  /**
   * Construct wapper an attach to a specified parent DOM node.
   *
   * @param  {Node} parent   Note to serve for attachign datepicker
   * @param  {Object} options Options to pass to datepicker
   */
  constructor(parent, options = null) {
    this._pickerElement = $(parent);
    /*
     * The widget is somewhat peculiar in that its functionality does
     * not exist until it has been called with a settings/options argument
     * as the only parameter to the datepicker() function.
     * So, unless some are passed in here explicitly, we create a default
     * and disabled date picker.
     */
    this._initWithOptions(options);
  }

  /**
   * Initialize the date picker with a set of options.
   *
   * Attach the datepicker function to its parent and set the specified options.
   * If the options are not specified a minimum set of options just enabling the
   * datepicker with defaults is used.
   *
   * @param  {Object} options Options for datepicker, or null to just enable
   */
  _initWithOptions(options = null) {
    this._alive = true;
    options =
      options !== null ?
        options :
        {
          disabled: true,
        };
    this._pickerElement.datepicker(options);
  }

  /**
   * Execute a specified datepicker function, passing the arguments.
   *
   * This function exists to catch the cases where functions are called when
   * the picker is not attached (alive).
   *
   * The first argument to this function should be the name of the desired
   * datepicker function, followed by the correct arguments for that function.
   *
   * @return {Any} Function result
   */
  _apply(...args) {
    if (!this._alive) {
      console.warn('datepicker not constructed yet [%s]', this.domId);
    }
    return this._pickerElement.datepicker(...args);
  }

  /**
   * Activate the datepicker if not already attached.
   *
   * Basically calls _initWithOptions({disabled: disabled}), but only if not
   * already attached. Otherwise just sets the disabled status.
   *
   * @param  {Boolean} disabled True if datepicker needs to be disabled
   */
  activate(disabled = true) {
    if (this._alive) {
      this.disabled = disabled;
    } else {
      this._initWithOptions({
        disabled: disabled,
      });
    }
  }

  /**
   * Get the element the datepicker is attached to.
   *
   * @return {jQuery} jQuery element
   */
  get element() {
    return this._pickerElement;
  }

  /**
   * Set option or options to the datepicker, like the 'option' call with
   * various arguments.
   *
   * Special case is when the datepicker is not (yet) attached. In that case
   * we need to initialze the datepicker with the options instead.
   *
   * @param {object} arg0   First parameter or undefined if not given
   * @param {array} args    Rest of the parameters (might be empty)
   * @return {object}       Result of the 'option' call.
   */
  option(arg0, ...args) {
    /*
     * Special case the scenario of calling option setting with just a map of
     * settings, when the picker is not alive. That really should translate
     * to a constructor call to the datepicker.
     */
    if (!this._alive && args.length === 0 && typeof arg0 === 'object') {
      return this._initWithOptions(arg0);
    }
    return this._apply('option', arg0, ...args);
  }

  /**
   * Return current set of options.
   *
   * This is special cased here vs. documentation. We need to ask for 'all'.
   *
   * @return {Object} Datepicker options
   */
  options() {
    return this.option('all');
  }

  /**
   * Determine whether datepicker is disabled.
   *
   * @return {Boolean}
   */
  get isDisabled() {
    return this._apply('isDisabled');
  }

  /**
   * Set disabled status of picker.
   *
   * @param  {Boolean} disabled True to disable
   */
  set disabled(disabled) {
    this.option('disabled', disabled);
  }

  /**
   * Get picker's currently selected date.
   *
   * @return {Date} Selected date
   */
  get date() {
    return this._apply('getDate');
  }

  /**
   * Set the datepicker to a specific date.
   *
   * @param  {String|Date} date Desired date as string or Date
   */
  set date(date) {
    this._apply('setDate', date);
  }

  /**
   * Get the picker's current date in ISO format.
   *
   * This will return just the date portion of the ISO-8601 format, or in other
   * words: YYYY-MM-DD
   *
   * @return {String} Date portion of ISO-8601 formatted selected date
   */
  get dateISO() {
    return this.date.toISOString().substr(0, 10);
  }

  /**
   * Get currently set minimum date.
   *
   * @return {Date} Minimum date
   */
  get minDate() {
    return this.option('minDate');
  }

  /**
   * Set a new minimum date.
   *
   * @param  {String|Date} value Desired minimum date
   */
  set minDate(value) {
    this.option('minDate', value);
  }

  /**
   * Get currently set maximum date.
   *
   * @return {Date} Maximum date
   */
  get maxDate() {
    return this.option('maxDate');
  }

  /**
   * Set a new maximum date.
   *
   * @param  {String|Date} value Desired maximum date
   */
  set maxDate(value) {
    this.option('maxDate', value);
  }

  /**
   * Set the picker to open up in a dialog.
   *
   * This takes a variable number of arguments, like the native dialog function.
   *
   * @param {varargs} dialogArgs Variable argument list
   */
  dialog(...dialogArgs) {
    this._apply('option', dialogArgs);
  }

  /**
   * Make the picker visible.
   */
  show() {
    this._apply('show');
  }

  /**
   * Hide the picker.
   */
  hide() {
    this._apply('hide');
  }

  /**
   * Refresh the picker.
   */
  refresh() {
    this._apply('refresh');
  }

  /**
   * Destroy the picker.
   *
   * Destroy means detach it from its element and dispose of everything.
   * Sets the status in this object to !alive.
   */
  destroy() {
    this._alive = true;
    this._apply('destroy');
    this._alive = false;
  }
}
