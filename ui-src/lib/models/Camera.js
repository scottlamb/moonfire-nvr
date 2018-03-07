// vim: set et sw=2:
//
//
import JsonWrapper from './JsonWrapper';
import Range90k from './Range90k';

/**
 * Camera JSON wrapper.
 */
export default class Camera extends JsonWrapper {
  /**
   * Construct from JSON.
   *
   * @param  {JSON} cameraJson JSON for single camera.
   */
  constructor(cameraJson) {
    super(cameraJson);
  }

  /**
   * Get camera uuid.
   *
   * @return {String} Camera's uuid
   */
  get uuid() {
    return this.json.uuid;
  }

  /**
   * Get camera's short name.
   *
   * @return {String} Name of the camera
   */
  get shortName() {
    return this.json.shortName;
  }

  /**
   * Get camera's description.
   *
   * @return {String} Camera's description
   */
  get description() {
    return this.json.description;
  }

  /**
   * Get maximimum amount of storage allowed to be used for camera's video
   * samples.
   *
   * @return {Number} Amount in bytes
   */
  get retainBytes() {
    return this.json.retainBytes;
  }

  /**
   * Get a Range90K object representing the range encompassing all available
   * video samples for the camera.
   *
   * This range does not mean every second of the range has video!
   *
   * @return {Range90k} [description]
   */
  get range90k() {
    return new Range90k(
      this.json.minStartTime90k,
      this.json.maxEndTime90k,
      this.json.totalDuration90k
    );
  }

  /**
   * Get the total amount of storage currently taken up by the camera's video
   * samples.
   *
   * @return {Number} Amount in bytes
   */
  get totalSampleFileBytes() {
    return this.json.totalSampleFileBytes;
  }

  /**
   * Get the list of the camera's days for which there are video samples.
   *
   * The result is a Map with dates as keys (in YYYY-MM-DD format) and each
   * value is a Range90k object for that day. Here too, the range does not
   * mean every second in the range has video, but presence of an entry for
   * a day does mean there is at least one (however short) video segment
   * available.
   *
   * @return {[type]} [description]
   */
  get days() {
    return new Map(
      Object.entries(this.json.days).map(function(t) {
        let [k, v] = t;
        v = new Range90k(v.startTime90k, v.endTime90k, v.totalDuration90k);
        return [k, v];
      })
    );
  }
}
