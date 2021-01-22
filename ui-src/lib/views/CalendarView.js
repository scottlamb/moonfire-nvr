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

import DatePickerView from './DatePickerView';
import CalendarTSRange from '../models/CalendarTSRange';
import TimeStamp90kFormatter from '../support/TimeStamp90kFormatter';
import Time90kParser from '../support/Time90kParser';

/**
 * Find the earliest and latest dates from an array of StreamView
 * objects.
 *
 * Each camera view has a "days" property, whose keys identify days with
 * recordings. All such dates are collected and then scanned to find earliest
 * and latest dates.
 *
 * "days" for camera views that are not enabled are ignored.
 *
 * @param  {[Iterable]} streamViews Camera views to look into
 * @return {[Set, String, String]}       Array with set of all dates, and
 *                                       earliest and latest dates
 */
function minMaxDates(streamViews) {
  /*
   * Produce a set with all dates, across all enabled cameras, that
   * have at least one recording available (allDates).
   */
  const allDates = new Set(
      [].concat(
          ...streamViews
              .filter((v) => v.enabled)
              .map((v) => Array.from(v.stream.days.keys()))
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
      this.fromPickerView_,
      this.toPickerView_,
      this.sameDayElement_,
      this.otherDayElement_,
      this.startTimeElement_,
      this.endTimeElement_,
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
    this.fromPickerView_ = new DatePickerView(this.fromPickerView_);
    this.toPickerView_ = new DatePickerView(this.toPickerView_);
    this.timeFormatter_ = new TimeStamp90kFormatter(timeZone);
    this.timeParser_ = new Time90kParser(timeZone);
    this.selectedRange_ = new CalendarTSRange(timeZone);
    this.sameDay_ = true; // Start in single day view
    this.sameDayElement_.prop('checked', this.sameDay_);
    this.otherDayElement_.prop('checked', !this.sameDay_);
    this.availableDates_ = null;
    this.minDateStr_ = null;
    this.maxDateStr_ = null;
    this.singleDateStr_ = null;
    this.streamViews_ = null;
    this.rangeChangedHandler_ = null;
  }

  /**
   * Change timezone.
   *
   * @param  {String} tz New timezone
   */
  set tz(tz) {
    this.timeParser_.tz = tz;
  }

  /**
   * (Re)configure the datepickers and other calendar range inputs to reflect
   * available dates.
   */
  configureDatePickers_() {
    const dateSet = this.availableDates_;
    const minDateStr = this.minDateStr_;
    const maxDateStr = this.maxDateStr_;
    const fromPickerView = this.fromPickerView_;
    const toPickerView = this.toPickerView_;
    const beforeShowDay = function(date) {
      const year = date.getYear() + 1900;
      const month = (date.getMonth() + 1).toString().padStart(2, '0');
      const day = date.getDate().toString().padStart(2, '0');
      const dateStr = [year, month, day].join('-');
      return [dateSet.has(dateStr), '', ''];
    };

    if (this.sameDay_) {
      fromPickerView.option({
        dateFormat: DatePickerView.datepicker.ISO_8601,
        minDate: minDateStr,
        maxDate: maxDateStr,
        defaultDate: maxDateStr,
        onSelect: (dateStr /* , picker */) =>
          this.updateRangeDates_(dateStr, dateStr),
        beforeShowDay: beforeShowDay,
        disabled: false,
      });
      toPickerView.destroy();
      toPickerView.activate(); // Default state, but alive
    } else {
      fromPickerView.option({
        dateFormat: DatePickerView.datepicker.ISO_8601,
        minDate: minDateStr,
        maxDate: maxDateStr,
        defaultDate: maxDateStr,
        onSelect: (dateStr /* , picker */) => {
          toPickerView.minDate = this.fromDateISO;
          this.updateRangeDates_(dateStr, this.toDateISO);
        },
        beforeShowDay: beforeShowDay,
        disabled: false,
      });
      toPickerView.option({
        dateFormat: DatePickerView.datepicker.ISO_8601,
        minDate: fromPickerView.dateISO,
        maxDate: maxDateStr,
        defaultDate: maxDateStr,
        onSelect: (dateStr /* , picker */) => {
          fromPickerView.maxDate = this.toDateISO;
          this.updateRangeDates_(this.fromDateISO, dateStr);
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
  informRangeChange_() {
    if (this.rangeChangedHandler_ !== null) {
      this.rangeChangedHandler_(this.selectedRange_);
    }
  }

  /**
   * Handle a change in the time input of either from or to.
   *
   * The change requires updating the selected range and then informing
   * any listeners that the range has changed (so they can update data).
   *
   * @param  {event}  event       DOM event that triggered us
   * @param  {Boolean} isEnd      True if this is for end time
   */
  pickerTimeChanged_(event, isEnd) {
    const pickerElement = event.currentTarget;
    const newTimeStr = pickerElement.value;
    const selectedRange = this.selectedRange_;
    const parsedTS = isEnd ?
      selectedRange.setEndTime(newTimeStr) :
      selectedRange.setStartTime(newTimeStr);
    if (parsedTS === null) {
      console.warn('bad time change');
      $(pickerElement).addClass('ui-state-error');
      return;
    }
    $(pickerElement).removeClass('ui-state-error');
    console.log(
        (isEnd ? 'End' : 'Start') +
        ' time changed to: ' +
        parsedTS +
        ' (' +
        this.timeFormatter_.formatTimeStamp90k(parsedTS) +
        ')'
    );
    this.informRangeChange_();
  }

  /**
   * Handle a change in the calendar's same/other day settings.
   *
   * The change means the selected range changes.
   *
   * @param {Boolean} isSameDay True if the same day radio button was activated
   */
  pickerSameDayChanged_(isSameDay) {
    // Prevent change if not real change (can happen on initial setup)
    if (this.sameDay_ != isSameDay) {
      /**
       * This is called when the status of the same/other day radio buttons
       * changes. We need to determine a new selected range and activiate it.
       * Doing so will then also inform the change listener.
       */
      const endDate = isSameDay ?
        this.selectedRange.start.dateStr :
        this.selectedRange.end.dateStr;
      this.updateRangeDates_(this.selectedRange.start.dateStr, endDate);
      this.sameDay_ = isSameDay;

      // Switch between single day and multi-day
      this.configureDatePickers_();
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
  updateRangeDates_(startDateStr, endDateStr) {
    const newRange = this.selectedRange_;
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
      this.informRangeChange_();
    }
  }

  /**
   * Install event handlers for same/other day radio buttons and the
   * time input boxes as both need to result in an update of the calendar
   * view.
   */
  wireControls_() {
    // If same day status changed, update the view
    this.sameDayElement_.change(() => this.pickerSameDayChanged_(true));
    this.otherDayElement_.change(() => this.pickerSameDayChanged_(false));

    // Handle changing of a time input (either from or to)
    const handler = (e, isEnd) => {
      console.log('Time changed for: ' + (isEnd ? 'end' : 'start'));
      this.pickerTimeChanged_(e, isEnd);
    };
    this.startTimeElement_.change((e) => handler(e, false));
    this.endTimeElement_.change((e) => handler(e, true));
  }

  /**
   * (Re)Initialize the calendar based on a collection of camera views.
   *
   * This is necessary as the camera views ultimately define the limits on
   * the date pickers.
   *
   * @param  {Iterable} streamViews Collection of camera views
   */
  initializeWith(streamViews) {
    this.streamViews_ = streamViews;
    [this.availableDates_, this.minDateStr_, this.maxDateStr_] = minMaxDates(
        streamViews
    );
    this.configureDatePickers_();

    // Initialize the selected range to the from picker's date
    // if we do not have a selected range yet
    if (!this.selectedRange.hasStart()) {
      const date = this.fromDateISO;
      this.updateRangeDates_(date, date);
      this.wireControls_();
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
    this.rangeChangedHandler_ = handler;
  }

  /**
   * Get the "to" selected date as Date object.
   *
   * @return {Date} Date value of the "to"date picker
   */
  get toDate() {
    return this.toPickerView_.date;
  }

  /**
   * Get the "from" selected date as Date object.
   *
   * @return {Date} Date value of the "from"date picker
   */
  get fromDate() {
    return this.fromPickerView_.date;
  }

  /**
   * Get the "to" selected date as the date component of an ISO-8601
   * formatted string.
   *
   * @return {String} Date value (YYYY-MM-D) of the "to" date picker
   */
  get toDateISO() {
    return this.toPickerView_.dateISO;
  }

  /**
   * Get the "from" selected date as the date component of an ISO-8601
   * formatted string.
   *
   * @return {String} Date value (YYYY-MM-D) of the "from" date picker
   */
  get fromDateISO() {
    return this.fromPickerView_.dateISO;
  }

  /**
   * Get the currently selected range in the calendar view.
   *
   * @return {CalendarTSRange} Range object
   */
  get selectedRange() {
    return this.selectedRange_;
  }
}
