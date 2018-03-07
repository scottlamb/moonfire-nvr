// vim: set et sw=2:
//

import $ from 'jquery';

/**
 * Class to encapsulate datepicker UI widget from jQuery.
 */
export default class DatePickerView {
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
      options !== null
        ? options
        : {
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
  _apply() {
    if (!this._alive) {
      console.log('WARN: datepicker not constructed yet [' + this.domId + ']');
    }
    return this._pickerElement.datepicker(...arguments);
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
   * @return {Any} Result of the 'option' call.
   */
  option() {
    /*
     * Special case the scenario of calling option setting with just a map of
     * settings, when the picker is not alive. That really should translate
     * to a constructor call to the datepicker.
     */
    if (
      !this._alive &&
      arguments.length == 1 &&
      (arguments[0] === null || typeof arguments[0] === 'object')
    ) {
      console.log('DP special case  [' + this.domId + ']: ', arguments[0]);
      return this._initWithOptions(arguments[0]);
    }
    return this._apply('option', ...arguments);
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
   * @return {Boolean} True if disabled.
   */
  get isDisabled() {
    return this._apply('isDisabled');
  }

  /**
   * Set dsiabled status of picker.
   *
   * @param  {Boolean} disabled True to disable
   */
  set disabled(dsiabled) {
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
