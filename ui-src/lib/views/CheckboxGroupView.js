// vim: set et sw=2:
//

import $ from 'jquery';

/**
 * Class to handle a group of (related) checkboxes.
 *
 * Each checkbox is managed through a simple object containing properties:
 * - id: {String} Id (some unique value within the group)
 * - selector: {String} jQuery compatible selector to find the dom element
 * - checked: {Boolean} Value for checkbox
 * - jq: {jQuery} jQuery element for the checkbox, or null if not found
 *
 * A handler can be called if a checbox changes value.
 */
export default class CheckboxGroupView {
  /**
   * Construct the seteup for the checkboxes.
   *
   * The passed group array should contain individual maps describing each
   * checkbox. THe maps should contain:
   * - id
   * - selector: optional. If not provided #id will be used
   * - checked: Initial value for checkbox, default true
   * - text: Text for the checkbox label (not generated if empty)
   *
   * @param  {Array}  group Array of maps, one for each checkbox
   * @param {jQuery} parent jQuery parent element to append to
   */
  constructor(group = [], parent = null) {
    this._group = group.slice(); // Copy
    this._group.forEach((element) => {
      // If parent specified, create and append
      if (parent) {
        let cb = `<input type="checkbox" id="${element.id}" name="${
          element.id
        }">`;
        if (element.text) {
          cb += `<label for="${element.id}">${element.text}</label>`;
        }
        parent.append($(cb + '<br/>'));
      }
      const jq = $(element.selector || `#${element.id}`);
      element.jq = jq;
      if (jq !== null) {
        jq.prop('checked', element.checked || true);
        jq.change((e) => {
          if (this._checkChangeHandler) {
            element.checked = e.target.checked;
            this._checkChangeHandler(element);
          }
        });
      }
    });
    this._checkChangeHandler = null;
  }

  /**
   * Get the checkbox object for the specified checkbox.
   *
   * The checkbox is looked up by the specified id or selector, which must
   * match what was specified during construction.
   *
   * @param {String} idOrSelector Identifying string
   * @return {Object} Object for checkbox, or null if not found
   */
  checkBox(idOrSelector) {
    return this._group.find(
      (el) => el.id === idOrSelector || el.selector === idOrSelector
    );
  }

  /**
   * Set a handler for checkbox changes.
   *
   * Handler will be called with same result as would be found by checkBox().
   *
   * @param  {Function} handler function (checbox)
   */
  set onCheckChange(handler) {
    this._checkChangeHandler = handler;
  }
}
