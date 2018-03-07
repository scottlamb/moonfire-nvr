// vim: set et sw=2:
//

import JsonWrapper from './JsonWrapper';
import Range90k from '../models/Range90k';

/**
 * Class to encapsulate recording JSON data.
 */
export default class Recording extends JsonWrapper {
  /**
   * Accept JSON data to be encapsulated
   *
   * @param  {object} recordingJson JSON for a recording
   */
  constructor(recordingJson) {
    super(recordingJson);
  }

  /**
   * Get recording's startId.
   *
   * @return {String} startId for recording
   */
  get startId() {
    return this.json.startId;
  }

  /**
   * Get recording's endId.
   *
   * @return {String} endId for recording
   */
  get endId() {
    return this.json.endId;
  }

  /**
   * Return start time of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get startTime90k() {
    return this.json.startTime90k;
  }

  /**
   * Return end time of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get endTime90k() {
    return this.json.endTime90k;
  }

  /**
   * Return duration of recording in 90k units.
   * @return {Number} Time in units of 90k parts of a second
   */
  get duration90k() {
    const data = this.json;
    return data.endTime90k - data.startTime90k;
  }

  /**
   * Compute the range of the recording in 90k timestamp units,
   * optionally trimmed by another range.
   *
   * @param  {Range90k} trimmedAgainst Optional range to trim against
   * @return {Range90k}                Resulting range
   */
  range90k(trimmedAgainst = null) {
    let result = new Range90k(
      this.startTime90k,
      this.endTime90k,
      this.duration90k
    );
    return trimmedAgainst ? result.trimmed(trimmedAgainst) : result;
  }
  /**
   * Return duration of recording in seconds.
   * @return {Number} Time in units of seconds.
   */
  get duration() {
    return this.duration90k / 90000;
  }

  /**
   * Get the number of bytes used by sample storage.
   *
   * @return {Number} Total bytes used
   */
  get sampleFileBytes() {
    return this.json.sampleFileBytes;
  }

  /**
   * Get the number of video samples (frames) for the recording.
   *
   * @return {Number} Total bytes used
   */
  get frameCount() {
    return this.json.videoSamples;
  }

  /**
   * Get the has for the video samples.
   *
   * @return {String} Hash
   */
  get videoSampleEntryHash() {
    return this.json.videoSampleEntrySha1;
  }

  /**
   * Get the width of the frame(s) of the video samples.
   *
   * @return {Number} Width in pixels
   */
  get videoSampleEntryWidth() {
    return this.json.videoSampleEntryWidth;
  }

  /**
   * Get the height of the frame(s) of the video samples.
   *
   * @return {Number} Height in pixels
   */
  get videoSampleEntryHeight() {
    return this.json.videoSampleEntryHeight;
  }
}
