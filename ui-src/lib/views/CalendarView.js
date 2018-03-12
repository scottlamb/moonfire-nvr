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

import $ from 'jquery';
import 'jquery-ui/themes/base/button.css';
import 'jquery-ui/themes/base/core.css';
import 'jquery-ui/themes/base/datepicker.css';
import 'jquery-ui/themes/base/dialog.css';
import 'jquery-ui/themes/base/resizable.css';
import 'jquery-ui/themes/base/theme.css';
import 'jquery-ui/themes/base/tooltip.css';
import 'jquery-ui/ui/widgets/datepicker';
import 'jquery-ui/ui/widgets/dialog';
import 'jquery-ui/ui/widgets/tooltip';

import DatePickerView from './DatePickerView';
import CalendarTSRange from '../models/CalendarTSRange';
import {TimeStamp90kFormatter} from '../support/TimeFormatter';
import Time90kParser from '../support/Time90kParser';

/**
 * Find the earliest and latest dates from an array of CameraView
 * objects.
 *
 * Each camera view has a "days" property, whose keys identify days with
 * recordings. All such dates are collected and then scanned to find earliest
 * and latest dates.
 *
 * "days" for camera views that are not enabled are ignored.
 *
 * @param  {[Iterable]} cameraViews Camera views to look into
 * @return {[Set, String, String]}       Array with set of all dates, and
 *                                       earliest and latest dates
 */
function minMaxDates(cameraViews) {
  /*
   * Produce a set with all dates, across all enabled cameras, that
   * have at least one recording available (allDates).
   */
  const allDates = new Set(
    [].concat(
      ...cameraViews
        .filter((v) => v.enabled)
        .map((v) => Array.from(v.camera.days.keys()))
    )
  );
  return [
    allDates,
    ...Array.from(allDates.values()).reduce((acc, dateStr) => {
      acc[0] = !acc[0] || dateStr < acc[0] ? dateStr : acc[0];
      acc[1] = !acc[1] || dateStr > acc[1] ? dateStr : acc[1];
      return acc;
    }, []),
  ];
}

/**
 * Class to represent a calendar view.
 *
 * The view consists of:
 * - Two date pickers (from and to)
 * - A time input box with each date picker (from time, to time)
 * - A set of radio buttons to select between same day or not
 *
 */
export default class CalendarView {
  /**
   * Construct the view with UI elements IDs specified.
   *
   * @param  {String} options.fromPickerId     Id for from datepicker
   * @param  {String} options.toPickerId       Id for to datepicker
   * @param  {String} options.isSameDayId      Id for same day radio button
   * @param  {String} options.isOtherDayId     Id for other day radio button
   * @param  {String} options.fromPickerTimeId Id for from time field
   * @param  {String} options.toPickerTimeId   Id for to time field
   * @param  {[type]} options.timeZone         Timezone
   */
  constructor({
    fromPickerId = 'start-date',
    toPickerId = 'end-date',
    isSameDayId = 'end-date-same',
    isOtherDayId = 'end-date-other',
    fromPickerTimeId = 'start-time',
    toPickerTimeId = 'end-time',
    timeZone = null,
  } = {}) {
    // Lookup all by id, check and remember
    [
      this._fromPickerView,
      this._toPickerView,
      this._sameDayElement,
      this._otherDayElement,
      this._startTimeElement,
      this._endTimeElement,
    ] = [
      fromPickerId,
      toPickerId,
      isSameDayId,
      isOtherDayId,
      fromPickerTimeId,
      toPickerTimeId,
    ].map((id) => {
      const el = $(`#${id}`);
      if (el.length == 0) {
        console.log('Warning: Calendar element with id "' + id + '" not found');
      }
      return el;
    });
    this._fromPickerView = new DatePickerView(this._fromPickerView);
    this._toPickerView = new DatePickerView(this._toPickerView);
    this._timeFormatter = new TimeStamp90kFormatter(timeZone);
    this._timeParser = new Time90kParser(timeZone);
    this._selectedRange = new CalendarTSRange(timeZone);
    this._sameDay = true; // Start in single day view
    this._sameDayElement.prop('checked', this._sameDay);
    this._otherDayElement.prop('checked', !this._sameDay);
    this._availableDates = null;
    this._minDateStr = null;
    this._maxDateStr = null;
    this._singleDateStr = null;
    this._cameraViews = null;
    this._rangeChangedHandler = null;
  }

  /**
   * Change timezone.
   *
   * @param  {String} tz New timezone
   */
  set tz(tz) {
    this._timeParser.tz = tz;
  }

  /**
   * (Re)configure the datepickers and other calendar range inputs to reflect
   * available dates.
   */
  _configureDatePickers() {
    const dateSet = this._availableDates;
    const minDateStr = this._minDateStr;
    const maxDateStr = this._maxDateStr;
    const fromPickerView = this._fromPickerView;
    const toPickerView = this._toPickerView;
    const beforeShowDay = function(date) {
      let dateStr = date.toISOString().substr(0, 10);
      return [dateSet.has(dateStr), '', ''];
    };

    if (this._sameDay) {
      fromPickerView.option({
        dateFormat: $.datepicker.ISO_8601,
        minDate: minDateStr,
        maxDate: maxDateStr,
        onSelect: (dateStr, picker) => this._updateRangeDates(dateStr, dateStr),
        beforeShowDay: beforeShowDay,
        disabled: false,
      });
      toPickerView.destroy();
      toPickerView.activate(); // Default state, but alive
    } else {
      fromPickerView.option({
        dateFormat: $.datepicker.ISO_8601,
        minDate: minDateStr,
        onSelect: (dateStr, picker) => {
          toPickerView.option('minDate', this.fromDateISO);
          this._updateRangeDates(dateStr, this.toDateISO);
        },
        beforeShowDay: beforeShowDay,
        disabled: false,
      });
      toPickerView.option({
        dateFormat: $.datepicker.ISO_8601,
        minDate: fromPickerView.dateISO,
        maxDate: maxDateStr,
        onSelect: (dateStr, picker) => {
          fromPickerView.option('maxDate', this.toDateISO);
          this._updateRangeDates(this.fromDateISO, dateStr);
        },
        beforeShowDay: beforeShowDay,
        disabled: false,
      });
      toPickerView.date = fromPickerView.date;
      fromPickerView.maxDate = toPickerView.date;
    }
  }

  /**
   * Call an installed handler (if any) to inform that range has changed.
   */
  _informRangeChange() {
    if (this._rangeChangedHandler !== null) {
      this._rangeChangedHandler(this._selectedRange);
    }
  }

  /**
   * Handle a change in the time input of either from or to.
   *
   * The change requires updating the selected range and then informing
   * any listeners that the range has changed (so they can update data).
   *
   * @param  {String}  newTimeStr Time string from input element
   * @param  {Boolean} isEnd      True if this is for end time
   */
  _pickerTimeChanged(event, isEnd) {
    const pickerElement = event.currentTarget;
    const newTimeStr = pickerElement.value;
    const selectedRange = this._selectedRange;
    const parsedTS = isEnd
      ? selectedRange.setEndTime(newTimeStr)
      : selectedRange.setStartTime(newTimeStr);
    if (parsedTS === null) {
      console.log('bad time change');
      $(pickerElement).addClass('ui-state-error');
      return;
    }
    $(pickerElement).removeClass('ui-state-error');
    console.log(
      (isEnd ? 'End' : 'Start') +
        ' time changed to: ' +
        parsedTS +
        ' (' +
        this._timeFormatter.formatTimeStamp90k(parsedTS) +
        ')'
    );
    this._informRangeChange();
  }

  /**
   * Handle a change in the calendar's same/other day settings.
   *
   * The change means the selected range changes.
   *
   * @param {Boolean} isSameDay True if the same day radio button was activated
   */
  _pickerSameDayChanged(isSameDay) {
    // Prevent change if not real change (can happen on initial setup)
    if (this._sameDay != isSameDay) {
      /**
       * This is called when the status of the same/other day radio buttons
       * changes. We need to determine a new selected range and activiate it.
       * Doing so will then also inform the change listener.
       */
      const endDate = isSameDay
        ? this.selectedRange.start.dateStr
        : this.selectedRange.end.dateStr;
      this._updateRangeDates(this.selectedRange.start.dateStr, endDate);
      this._sameDay = isSameDay;

      // Switch between single day and multi-day
      this._configureDatePickers();
    }
  }

  /**
   * Reflect a change in start and end date in the calendar view.
   *
   * The selected range is update, the view is reconfigured as necessary and
   * any listeners are informed.
   *
   * @param  {String} startDateStr New starting date
   * @param  {String} endDateStr   New ending date
   */
  _updateRangeDates(startDateStr, endDateStr) {
    const newRange = this._selectedRange;
    const originalStart = Object.assign({}, newRange.start);
    const originalEnd = Object.assign({}, newRange.end);
    newRange.setStartDate(startDateStr);
    newRange.setEndDate(endDateStr);

    const isSameRange = (a, b) => {
      return (
        a.dateStr == b.dateStr && a.timeStr == b.timeStr && a.ts90k == b.ts90k
      );
    };

    // Do nothing if effectively no change
    if (
      !isSameRange(newRange.start, originalStart) ||
      !isSameRange(newRange.end, originalEnd)
    ) {
      console.log('New range: ' + startDateStr + ' - ' + endDateStr);
      this._informRangeChange();
    }
  }

  /**
   * Install event handlers for same/other day radio buttons and the
   * time input boxes as both need to result in an update of the calendar
   * view.
   */
  _wireControls() {
    // If same day status changed, update the view
    this._sameDayElement.change(() => this._pickerSameDayChanged(true));
    this._otherDayElement.change(() => this._pickerSameDayChanged(false));

    // Handle changing of a time input (either from or to)
    const handler = (e, isEnd) => {
      console.log('Time changed for: ' + (isEnd ? 'end' : 'start'));
      this._pickerTimeChanged(e, isEnd);
    };
    this._startTimeElement.change((e) => handler(e, false));
    this._endTimeElement.change((e) => handler(e, true));
  }

  /**
   * (Re)Initialize the calendar based on a collection of camera views.
   *
   * This is necessary as the camera views ultimately define the limits on
   * the date pickers.
   *
   * @param  {Iterable} cameraViews Collection of camera views
   */
  initializeWith(cameraViews) {
    this._cameraViews = cameraViews;
    [this._availableDates, this._minDateStr, this._maxDateStr] = minMaxDates(
      cameraViews
    );
    this._configureDatePickers();

    // Initialize the selected range to the from picker's date
    // if we do not have a selected range yet
    if (!this.selectedRange.hasStart()) {
      const date = this.fromDateISO;
      this._updateRangeDates(date, date);
      this._wireControls();
    }
  }

  /**
   * Set a handler to be called when the calendar selection range changes.
   *
   * The handler will be called with one argument, an object of type
   * CalendarTSRange reflecting the current calendar range. It will be called
   * whenever that range changes.
   *
   * @param  {Function} handler Function that will be called
   */
  set onRangeChange(handler) {
    this._rangeChangedHandler = handler;
  }

  /**
   * Get the "to" selected date as Date object.
   *
   * @return {Date} Date value of the "to"date picker
   */
  get toDate() {
    return this._toPickerView.date;
  }

  /**
   * Get the "from" selected date as Date object.
   *
   * @return {Date} Date value of the "from"date picker
   */
  get fromDate() {
    return this._fromPickerView.date;
  }

  /**
   * Get the "to" selected date as the date component of an ISO-8601
   * formatted string.
   *
   * @return {String} Date value (YYYY-MM-D) of the "to" date picker
   */
  get toDateISO() {
    return this._toPickerView.dateISO;
  }

  /**
   * Get the "from" selected date as the date component of an ISO-8601
   * formatted string.
   *
   * @return {String} Date value (YYYY-MM-D) of the "from" date picker
   */
  get fromDateISO() {
    return this._fromPickerView.dateISO;
  }

  /**
   * Get the currently selected range in the calendar view.
   *
   * @return {CalendarTSRange} Range object
   */
  get selectedRange() {
    return this._selectedRange;
  }
}
