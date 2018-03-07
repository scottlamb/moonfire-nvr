// vim: set et sw=2:
//

import TimeFormatter from './TimeFormatter';

/**
 * Formatter for framerates
 * @type {Intl} Formatter
 */
const frameRateFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 0,
});

/**
 * Formatter for sizes
 * @type {Intl} Formatter
 */
const sizeFmt = new Intl.NumberFormat([], {
  maximumFractionDigits: 1,
});

/**
 * Class encapsulating formatting of recording time ranges.
 */
export default class RecordingFormatter {
  /**
   * Construct with desired time format and timezone.
   *
   * @param  {String} formatStr Time format string
   * @param  {String} tz        Timezone
   */
  constructor(formatStr, tz) {
    this._timeFormatter = new TimeFormatter(formatStr, tz);
    this._singleDateStr = null;
  }

  /**
   * Change time format string, preserving timezone.
   *
   * @param  {String} formatStr Time format string
   */
  set timeFormat(formatStr) {
    this._timeFormatter = new TimeFormatter(formatStr, this._timeFormatter.tz);
  }

  /**
   * [format description]
   * @param  {Recording} recording    Recording to be formatted
   * @param  {Range90k} trimRange     Optional time range for trimming the
   *                                  recording's interval
   * @return {Object}                 Map, keyed by _columnOrder element
   */
  format(recording, trimRange = null) {
    const duration = recording.duration;
    const trimmedRange = recording.range90k(trimRange);
    return {
      start: this._timeFormatter.formatTimeStamp90k(trimmedRange.startTime90k),
      end: this._timeFormatter.formatTimeStamp90k(trimmedRange.endTime90k),
      resolution:
        recording.videoSampleEntryWidth +
        'x' +
        recording.videoSampleEntryHeight,
      frameRate: frameRateFmt.format(recording.frameCount / duration),
      size: sizeFmt.format(recording.sampleFileBytes / 1048576) + ' MB',
      rate:
        sizeFmt.format(recording.sampleFileBytes / duration * 0.000008) +
        ' Mbps',
    };
  }
}
