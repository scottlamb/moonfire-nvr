// vim: set et sw=2:
//

import $ from 'jquery';
import URLBuilder from './support/URLBuilder';

/**
 * Class to insulate rest of app from details of Moonfire API.
 *
 * Can produce URLs for specifc operations, or a request that has been
 * started and can have handlers attached.
 */
export default class MoonfireAPI {
  /**
   * Construct.
   *
   * The defaults correspond to a standard Moonfire installation on the
   * same host that this code runs on.
   *
   * Requesting relative URLs effectively disregards the host and port options.
   *
   * @param  {String} options.host         Host where the API resides
   * @param  {Number} options.port         Port on which the API resides
   * @param  {[type]} options.relativeUrls True if we should produce relative urls
   */
  constructor({host = 'localhost', port = 8080, relativeUrls = true} = {}) {
    const url = new URL('/api/', 'http://acme.com');
    url.protocol = 'http:';
    url.hostname = host;
    url.port = port;
    console.log('API: ' + url.origin + url.pathname);
    this._builder = new URLBuilder(url.origin + url.pathname);
  }

  /**
   * URL that will cause the state of the NVR to be returned.
   *
   * @param  {Boolean} days True of a return of days with available recordings
   *                        is desired.
   * @return {String}       Constructed url
   */
  nvrUrl(days = true) {
    return this._builder.makeUrl('', {days: days});
  }

  /**
   * URL that will cause the state of a specificto be returned.
   *
   * @param  {String} cameraUUID UUID for the camera
   * @param  {String} start90k   Timestamp for beginning of range of interest
   * @param  {String} end90k     Timestamp for end of range of interest
   * @param  {String} split90k   Desired maximum size of segments returned
   * @return {String}       Constructed url
   */
  recordingsUrl(cameraUUID, start90k, end90k, split90k = null) {
    const query = {
      startTime90k: start90k,
      endTime90k: end90k,
    };
    if (split) {
      query.split90k = split90k;
    }
    return this._builder.makeUrl(
      'cameras/' + cameraUUID + '/recordings',
      query
    );
  }

  /**
   * URL that will playback a video segment.
   *
   * @param  {String} cameraUUID UUID for the camera from whence comes the video
   * @param  {Recording}  recording     Recording model object
   * @param  {Range90k}  trimmedRange   Range restricting segments
   * @param  {Boolean} timestampTrack   True if track should be timestamped
   * @return {String}                 Constructed url
   */
  videoPlayUrl(cameraUUID, recording, trimmedRange, timestampTrack = true) {
    let sParam = recording.startId;
    if (recording.endId !== undefined) {
      sParam += '-' + recording.endId;
    }
    let rel = '';
    if (recording.startTime90k < trimmedRange.startTime90k) {
      rel += trimmedRange.startTime90k - recording.startTime90k;
    }
    rel += '-';
    if (recording.endTime90k > trimmedRange.endTime90k) {
      rel += trimmedRange.endTime90k - recording.startTime90k;
    }
    if (rel !== '-') {
      sParam += '.' + rel;
    }
    console.log('Video query:', {
      s: sParam,
      ts: timestampTrack,
    });
    return this._builder.makeUrl('cameras/' + cameraUUID + '/view.mp4', {
      s: sParam,
      ts: timestampTrack,
    });
  }

  /**
   * Start a new AJAX request with the specified URL.
   *
   * @param  {String} url     URL to use
   * @param  {String} cacheOk True if cached results are OK
   * @return {Request}        jQuery request type
   */
  request(url, cacheOk = false) {
    return $.ajax(url, {
      dataType: 'json',
      headers: {
        Accept: 'application/json',
      },
      cache: cacheOk,
    });
  }
}
