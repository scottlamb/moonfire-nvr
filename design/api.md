# Moonfire NVR API <!-- omit in toc -->

Status: **current**.

* [Objective](#objective)
* [Detailed design](#detailed-design)
    * [`POST /api/login`](#post-apilogin)
    * [`POST /api/logout`](#post-apilogout)
    * [`GET /api/`](#get-api)
    * [`GET /api/cameras/<uuid>/`](#get-apicamerasuuid)
    * [`GET /api/cameras/<uuid>/<stream>/recordings`](#get-apicamerasuuidstreamrecordings)
    * [`GET /api/cameras/<uuid>/<stream>/view.mp4`](#get-apicamerasuuidstreamviewmp4)
    * [`GET /api/cameras/<uuid>/<stream>/view.mp4.txt`](#get-apicamerasuuidstreamviewmp4txt)
    * [`GET /api/cameras/<uuid>/<stream>/view.m4s`](#get-apicamerasuuidstreamviewm4s)
    * [`GET /api/cameras/<uuid>/<stream>/view.m4s.txt`](#get-apicamerasuuidstreamviewm4stxt)
    * [`GET /api/cameras/<uuid>/<stream>/live.m4s`](#get-apicamerasuuidstreamlivem4s)
    * [`GET /api/init/<id>.mp4`](#get-apiinitidmp4)
    * [`GET /api/init/<id>.mp4.txt`](#get-apiinitidmp4txt)
    * [`GET /api/signals`](#get-apisignals)
    * [`POST /api/signals`](#post-apisignals)
        * [Request 1](#request-1)
        * [Request 2](#request-2)
        * [Request 3](#request-3)

## Objective

Allow a JavaScript-based web interface to list cameras and view recordings.
Support external analytics.

In the future, this is likely to be expanded:

*   configuration support
*   commandline tool over a UNIX-domain socket
    (at least for bootstrapping web authentication)

## Detailed design

*Note:* italicized terms in this document are defined in the [glossary](glossary.md).

All requests for JSON data should be sent with the header
`Accept: application/json` (exactly).

### `POST /api/login`

The request should have an `application/json` body containing a dict with
`username` and `password` keys.

On successful authentication, the server will return an HTTP 204 (no content)
with a `Set-Cookie` header for the `s` cookie, which is an opaque, `HttpOnly`
(unavailable to Javascript) session identifier.

If authentication or authorization fails, the server will return a HTTP 403
(forbidden) response. Currently the body will be a `text/plain` error message;
future versions will likely be more sophisticated.

### `POST /api/logout`

The request should have an `application/json` body containing
a `csrf` parameter copied from the `session.csrf` of the
top-level API request.

On success, returns an HTTP 204 (no content) responses. On failure, returns a
4xx response with `text/plain` error message.

### `GET /api/`

Returns basic information about the server, including all cameras. Valid
request parameters:

*   `days`: a boolean indicating if the days parameter described below
    should be included.
*   `cameraConfigs`: a boolean indicating if the `camera.config` and
    `camera.stream[].config` parameters described below should be included.
    This requires the `read_camera_configs` permission as described in
    `schema.proto`.

Example request URI (with added whitespace between parameters):

```
/api/?days=true
     &cameraConfigs=true
```

The `application/json` response will have a dict as follows:

*   `timeZoneName`: the name of the IANA time zone the server is using
    to divide recordings into days as described further below.
*   `cameras`: a list of cameras. Each is a dict as follows:
    *   `uuid`: in text format
    *   `shortName`: a short name (typically one or two words)
    *   `description`: a longer description (typically a phrase or paragraph)
    *   `config`: (only included if request parameter `cameraConfigs` is true)
        a dictionary describing the configuration of the camera:
        *   `username`
        *   `password`
        *   `onvif_host`
    *   `streams`: a dict of stream type ("main" or "sub") to a dictionary
        describing the stream:
        *   `retainBytes`: the configured total number of bytes of completed
            recordings to retain.
        *   `minStartTime90k`: the start time of the earliest recording for
            this camera, in 90kHz units since 1970-01-01 00:00:00 UTC.
        *   `maxEndTime90k`: the end time of the latest recording for this
            camera, in 90kHz units since 1970-01-01 00:00:00 UTC.
        *   `totalDuration90k`: the total duration recorded, in 90 kHz units.
            This is no greater than `maxEndTime90k - maxStartTime90k`; it will
            be lesser if there are gaps in the recorded data.
        *   `totalSampleFileBytes`: the total number of bytes of sample data
            (the `mdat` portion of a `.mp4` file).
        *   `fsBytes`: the total number of bytes on the filesystem used by
            this stream. This is slightly more than `totalSampleFileBytes`
            because it also includes the wasted portion of the final
            filesystem block allocated to each file.
        *   `days`: (only included if request parameter `days` is true)
            dictionary representing calendar days (in the server's time zone)
            with non-zero total duration of recordings for that day. Currently
            this includes uncommitted and growing recordings. This is likely
            to change in a future release for
            [#40](https://github.com/scottlamb/moonfire-nvr/issues/40). The
            keys are of the form `YYYY-mm-dd`; the values are objects with the
            following attributes:
            *   `totalDuration90k` is the total duration recorded during that
                day.  If a recording spans a day boundary, some portion of it
                is accounted to each day.
            *   `startTime90k` is the start of that calendar day in the
                server's time zone.
            *   `endTime90k` is the end of that calendar day in the server's
                time zone.  It is usually 24 hours after the start time. It
                might be 23 hours or 25 hours during spring forward or fall
                back, respectively.
        *   `config`: (only included if request parameter `cameraConfigs` is
            true) a dictionary describing the configuration of the stream:
            *   `rtsp_url`
*   `signals`: a list of all *signals* known to the server. Each is a dictionary
    with the following properties:
    *   `id`: an integer identifier.
    *   `shortName`: a unique, human-readable description of the signal
    *   `cameras`: a map of associated cameras' UUIDs to the type of association:
        `direct` or `indirect`. See `db/schema.sql` for more description.
    *   `type`: a UUID, expected to match one of `signalTypes`.
    *   `days`: (only included if request parameter `days` is true) similar to
        `cameras.days` above. Values are objects with the following attributes:
        *   `states`: an array of the time the signal is in each state, starting
            from 1. These may not sum to the entire day; if so, the rest of the
            day is in state 0 (`unknown`).
*   `signalTypes`: a list of all known signal types.
    *   `uuid`: in text format.
    *   `states`: a map of all possible states of the enumeration to more
        information about them:
        *   `color`: a recommended color to use in UIs to represent this state,
            as in the [HTML specification](https://html.spec.whatwg.org/#colours).
        *   `motion`: if present and true, directly associated cameras will be
            considered to have motion when this signal is in this state.
*   `session`: if logged in, a dict with the following properties:
    *   `username`
    *   `csrf`: a cross-site request forgery token for use in `POST` requests.

Example response:

```json
{
  "timeZoneName": "America/Los_Angeles",
  "cameras": [
    {
      "uuid": "fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe",
      "shortName": "driveway",
      "description": "Hikvision DS-2CD2032 overlooking the driveway from east",
      "config": {
        "onvif_host": "192.168.1.100",
        "user": "admin",
        "password": "12345",
      },
      "streams": {
        "main": {
          "retainBytes": 536870912000,
          "minStartTime90k": 130888729442361,
          "maxEndTime90k": 130985466591817,
          "totalDuration90k": 96736169725,
          "totalSampleFileBytes": 446774393937,
          "record": true,
          "days": {
            "2016-05-01": {
              "endTime90k": 131595516000000,
              "startTime90k": 131587740000000,
              "totalDuration90k": 52617609
            },
            "2016-05-02": {
              "endTime90k": 131603292000000,
              "startTime90k": 131595516000000,
              "totalDuration90k": 20946022
            }
          }
        }
      }
    },
    ...
  ],
  "signals": [
    {
      "id": 1,
      "shortName": "driveway motion",
      "cameras": {
        "fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe": "direct"
      },
      "type": "ee66270f-d9c6-4819-8b33-9720d4cbca6b",
      "days": {
        "2016-05-01": {
           "endTime90k": 131595516000000,
           "startTime90k": 131587740000000,
           "states": [5400000]
         }
       }
    }
  ],
  "signalTypes": [
    {
      "uuid": "ee66270f-d9c6-4819-8b33-9720d4cbca6b",
      "states": {
        0: {
          "name": "unknown",
          "color": "#000000"
        },
        1: {
          "name": "off",
          "color": "#888888"
        },
        2: {
          "name": "on",
          "color": "#ff8888",
          "motion": true
        }
      }
    }
  ],
  "session": {
    "username": "slamb",
    "csrf": "2DivvlnKUQ9JD4ao6YACBJm8XK4bFmOc"
  }
}
```

### `GET /api/cameras/<uuid>/`

Returns information for the camera with the given URL. As in the like section
of `GET /api/` with the `days` parameter set and the `cameraConfigs` parameter
unset.

Example response:

```json
{
  "description": "",
  "streams": {
    "main": {
      "days": {
        "2016-05-01": {
          "endTime90k": 131595516000000,
          "startTime90k": 131587740000000,
          "totalDuration90k": 52617609
        },
        "2016-05-02": {
          "endTime90k": 131603292000000,
          "startTime90k": 131595516000000,
          "totalDuration90k": 20946022
        }
      },
      "maxEndTime90k": 131598273666690,
      "minStartTime90k": 131590386129355,
      "retainBytes": 104857600,
      "totalDuration90k": 73563631,
      "totalSampleFileBytes": 98901406
    }
  },
  "shortName": "driveway"
}
```

### `GET /api/cameras/<uuid>/<stream>/recordings`

Returns information about *recordings*. Valid request parameters:

*   `startTime90k` and and `endTime90k` limit the data returned to only
    recordings with wall times overlapping with the given half-open interval.
    Either or both may be absent; they default to the beginning and end of time,
    respectively.
*   `split90k` causes long runs of recordings to be split at the next
    convenient boundary after the given duration.
*   TODO(slamb): `continue` to support paging. (If data is too large, the
    server should return a `continue` key which is expected to be returned on
    following requests.)

Returns a JSON object. Under the key `recordings` is an array of recordings in
arbitrary order. Each recording object has the following properties:

*   `startId`. The id of this recording, which can be used with `/view.mp4`
    to retrieve its content.
*   `endId` (optional). If absent, this object describes a single recording.
    If present, this indicates that recordings `startId-endId` (inclusive)
    together are as described. Adjacent recordings from the same RTSP session
    may be coalesced in this fashion to reduce the amount of redundant data
    transferred.
*   `firstUncommitted` (optional). If this range is not fully committed to the
    database, the first id that is uncommitted. This is significant because
    it's possible that after a crash and restart, this id will refer to a
    completely different recording. That recording will have a different
    `openId`.
*   `growing` (optional). If this boolean is true, the recording `endId` is
    still being written to. Accesses to this id (such as `view.mp4`) may
    retrieve more data than described here if not bounded by duration.
    Additionally, if `startId` == `endId`, the start time of the recording is
    "unanchored" and may change in subsequent accesses.
*   `openId`. Each time Moonfire NVR starts in read-write mode, it is assigned
    an increasing "open id". This field is the open id as of when these
    recordings were written. This can be used to disambiguate ids referring to
    uncommitted recordings.
*   `startTime90k`: the start time of the given recording, in the wall time
    scale. Note this may be less than the requested `startTime90k` if this
    recording was ongoing at the requested time.
*   `endTime90k`: the end time of the given recording, in the wall time scale.
    Note this may be greater than the requested `endTime90k` if this recording
    was ongoing at the requested time.
*   `videoSampleEntryId`: a reference to an entry in the `videoSampleEntries`
    object.
*   `videoSamples`: the number of samples (aka frames) of video in this
    recording.
*   `sampleFileBytes`: the number of bytes of video in this recording.

Under the property `videoSampleEntries`, an object mapping ids to objects with
the following properties:

*   `width`: the stored width in pixels.
*   `height`: the stored height in pixels.
*   `pixelHSpacing`: the relative width of a pixel, as in a ISO/IEC 14496-12
    section 12.1.4.3 `PixelAspectRatioBox`. If absent, assumed to be 1.
*   `pixelVSpacing`: the relative height of a pixel, as in a ISO/IEC 14496-12
    section 12.1.4.3 `PixelAspectRatioBox`. If absent, assumed to be 1.
*   `aspectWidth`: the width component of the aspect ratio. (The aspect ratio
    can be computed from the dimensions and pixel spacing; it's included as a
    convenience.)
*   `aspectHeight`: the height component of the aspect ratio.

The full initialization segment data for a given video sample entry can be
retrieved at the URL `/api/init/<id>.mp4`.

Example request URI (with added whitespace between parameters):

```
/api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/recordings
    ?startTime90k=130888729442361
    &endTime90k=130985466591817
```

Example response:

```json
{
  "recordings": [
    {
      "startId": 1,
      "startTime90k": 130985461191810,
      "endTime90k": 130985466591817,
      "sampleFileBytes": 8405564,
      "videoSampleEntryId": 1,
    },
    {
      "endTime90k": 130985461191810,
      ...
    },
    ...
  ],
  "videoSampleEntries": {
    "1": {
      "width": 1280,
      "height": 720
    }
  },
}
```

### `GET /api/cameras/<uuid>/<stream>/view.mp4`

Requires the `view_video` permission.

Returns a `.mp4` file, with an etag and support for range requests. The MIME
type will be `video/mp4`, with a `codecs` parameter as specified in
[RFC 6381][rfc-6381].

Expected query parameters:

*   `s` (one or more): a string of the form
    `START_ID[-END_ID][@OPEN_ID][.[REL_START_TIME]-[REL_END_TIME]]`. This
    specifies *segments* to include. The produced `.mp4` file will be a
    concatenation of the segments indicated by all `s` parameters. The ids to
    retrieve are as returned by the `/recordings` URL.  The *open id* is
    optional and will be enforced if present; it's recommended for
    disambiguation when the requested range includes uncommitted recordings.
    The optional start and end times are in 90k units of wall time and relative
    to the start of the first specified id. These can be used to clip the
    returned segments. Note they can be used to skip over some ids entirely;
    this is allowed so that the caller doesn't need to know the start time of
    each interior id. If there is no key frame at the desired relative start
    time, frames back to the last key frame will be included in the returned
    data, and an edit list will instruct the viewer to skip to the desired
    start time.
*   `ts` (optional): should be set to `true` to request a subtitle track be
    added with human-readable recording timestamps.

Example request URI to retrieve all of recording id 1 from the given camera:

```
    /api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.mp4?s=1
```

Example request URI to retrieve all of recording ids 1–5 from the given camera,
with timestamp subtitles:

```
    /api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.mp4?s=1-5&ts=true
```

Example request URI to retrieve recording id 1, skipping its first 26
90,000ths of a second:

```
    /api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.mp4?s=1.26
```

Note carefully the distinction between *wall duration* and *media duration*.
It's normal for `/view.mp4` to return a media presentation with a length
slightly different from the *wall duration* of the backing recording or
portion that was requested.

TODO: error behavior on missing segment. It should be a 404, likely with an
`application/json` body describing what portion if any (still) exists.

### `GET /api/cameras/<uuid>/<stream>/view.mp4.txt`

Returns a `text/plain` debugging string for the `.mp4` generated by the
same URL minus the `.txt` suffix.

### `GET /api/cameras/<uuid>/<stream>/view.m4s`

Returns a `.mp4` suitable for use as a [HTML5 Media Source Extensions
media segment][media-segment]. The MIME type will be `video/mp4`, with a
`codecs` parameter as specified in [RFC 6381][rfc-6381]. Note that these
can't include edit lists, so (unlike `/view.mp4`) the caller must manually
trim undesired leading portions.

This response will include the following additional headers:

*   `X-Prev-Media-Duration`: the total *media duration* (in 90 kHz units) of all
    *recordings* before the first requested recording in the `s` parameter.
    Browser-based callers may use this to place this at the correct position in
    the source buffer via `SourceBuffer.timestampOffset`.
*   `X-Runs`: the cumulative number of "runs" of recordings. If this recording
    starts a new run, it is included in the count. Browser-based callers may
    use this to force gaps in the source buffer timeline by adjusting the
    timestamp offset if desired.
*   `X-Leading-Media-Duration`: if present, the total duration (in 90 kHz
    units) of additional leading video included before the caller's first
    requested timestamp. This happens when the caller's requested timestamp
    does not fall exactly on a key frame. Media segments can't include edit
    lists, so unlike with the `/api/.../view.mp4` endpoint the caller is
    responsible for trimming this portion. Browser-based callers may use
    `SourceBuffer.appendWindowStart`.

Expected query parameters:

*   `s` (one or more): as with the `.mp4` URL.

It's recommended that each `.m4s` retrieval be for at most one Moonfire NVR
recording segment. The fundamental reason is that the Media Source Extension
API appears structured for adding a complete segment at a time. Large media
segments thus impose significant latency on seeking. Additionally, because of
this fundamental reason Moonfire NVR makes no effort to make multiple-segment
`.m4s` requests practical:

*   There is currently a hard limit of 4 GiB of data because the `.m4s` uses a
    single `moof` followed by a single `mdat`; the former references the
    latter with 32-bit offsets.
*   There's currently no way to generate an initialization segment for more
    than one video sample entry, so a `.m4s` that uses more than one video
    sample entry can't be used.
*   The `X-Prev-Media-Duration` and `X-Leading-Media-Duration` headers only
    describe the first segment.

Timestamp tracks (see the `ts` parameter to `.mp4` URIs) aren't supported
today. Most likely browser clients will implement timestamp subtitles via
WebVTT API calls anyway.

### `GET /api/cameras/<uuid>/<stream>/view.m4s.txt`

Returns a `text/plain` debugging string for the `.mp4` generated by the same
URL minus the `.txt` suffix.

### `GET /api/cameras/<uuid>/<stream>/live.m4s`

Initiate a WebSocket stream for chunks of video. Expects the standard
WebSocket headers as described in [RFC 6455][rfc-6455] and (if authentication
is required) the `s` cookie.

The server will send a sequence of binary messages. Each message corresponds
to one or more frames of video. The first message is guaranteed to start with a
"key" (IDR) frame; others may not. The message will contain HTTP headers
followed by by a `.mp4` media segment. The following headers will be included:

*   `X-Recording-Id`: the open id, a period, and the recording id of the
    recording these frames belong to.
*   `X-Recording-Start`: the timestamp (in Moonfire NVR's usual 90,000ths
    of a second) of the start of the recording. Note that if the recording
    is "unanchored" (as described in `GET /api/.../recordings`), the
    recording's start time may change before it is completed.
*   `X-Prev-Media-Duration`: as in `/.../view.m4s`.
*   `X-Runs`: as in `/.../view.m4s`.
*   `X-Media-Time-Range`: the relative media start and end times of these
    frames within the recording, as a half-open interval.

The server will also send pings, currently at 30-second intervals.

The WebSocket will always open immediately but will receive messages only while the
backing RTSP stream is connected.

Example request URI:

```
/api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/live.m4s
```

Example binary message sequence:

```
Content-Type: video/mp4; codecs="avc1.640028"
X-Recording-Id: 42.5680
X-Recording-Start: 130985461191810
X-Prev-Media-Duration: 10000000
X-Media-Time-Range: 5220058-5400061
X-Video-Sample-Entry-Id: 4

binary mp4 data
```

```
Content-Type: video/mp4; codecs="avc1.640028"
X-Recording-Id: 42.5681
X-Recording-Start: 130985461191822
X-Prev-Media-Duration: 10180003
X-Media-Time-Range: 0-180002
X-Video-Sample-Entry-Id: 4

binary mp4 data
```

```
Content-Type: video/mp4; codecs="avc1.640028"
X-Recording-Id: 42.5681
X-Recording-Start: 130985461191822
X-Prev-Media-Duration: 10360005
X-Media-Time-Range: 180002-360004
X-Video-Sample-Entry-Id: 4

binary mp4 data
```

These roughly correspond to the `.m4s` files available at the following URLs:

*   `/api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.m4s?s=5680@42.5220058-5400061`
*   `/api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.m4s?s=5681@42.0-180002`
*   `/api/cameras/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/main/view.m4s?s=5681@42.180002-360004`

However, there are two important differences:

*   The `/view.m4s` endpoint accepts offsets within a recording as wall durations;
    the `/live.m4s` endpoint's `X-Media-Time-Range` header returns them as
    media durations. Thus the URLs above are only exactly correct if the wall
    and media durations of the recording are identical.
*   The `/view.m4s` endpoint always returns a time range that starts with a key frame;
    `/live.m4s` messages may not include a key frame.

Note: an earlier version of this API used a `multipart/mixed` segment instead,
compatible with the [multipart-stream-js][multipart-stream-js] library. The
problem with this approach is that browsers have low limits on the number of
active HTTP/1.1 connections: six in Chrome's case. The WebSocket limit is much
higher (256), allowing browser-side Javascript to stream all active camera
streams simultaneously as well as making other simultaneous HTTP requests.

### `GET /api/init/<id>.mp4`

Returns a `.mp4` suitable for use as a [HTML5 Media Source Extensions
initialization segment][init-segment]. The MIME type will be `video/mp4`, with
a `codecs` parameter as specified in [RFC 6381][rfc-6381].

An `X-Aspect` HTTP header will include the aspect ratio as width:height,
eg `16:9` (most cameras) or `9:16` (rotated 90 degrees).
This is redundant with the returned `.mp4` but is far easier to parse from
Javascript.

### `GET /api/init/<id>.mp4.txt`

Returns a `text/plain` debugging string for the `.mp4` generated by the
same URL minus the `.txt` suffix.

### `GET /api/signals`

Returns an `application/json` response with state of every signal for the
requested timespan.

Valid request parameters:

*   `startTime90k` and and `endTime90k` limit the data returned to only
    events relevant to the given half-open interval. Either or both
    may be absent; they default to the beginning and end of time, respectively.
    This will return the current state as of the latest change (to any signal)
    before the start time (if any), then all changes in the interval. This
    allows the caller to determine the state at every moment during the
    selected timespan, as well as observe all events.

Responses are several parallel arrays for each observation:

  * `times90k`: the time of each event. Events are given in ascending order.
  * `signalIds`: the id of the relevant signal; expected to match one in the
    `signals` field of the `/api/` response.
  * `states`: the new state.

Example request URI (with added whitespace between parameters):

```
/api/signals
    ?startTime90k=130888729442361
    &endTime90k=130985466591817
```

Example response:

```json
{
  "signalIds": [1, 1, 1],
  "states": [1, 2, 1],
  "times90k": [130888729440000, 130985424000000, 130985418600000]
}
```

This represents the following observations:

  1. time 130888729440000 was the last change before the requested start;
     signal 1 (`driveway motion`) was in state 1 (`off`).
  2. signal 1 entered state 2 (`on`) at time 130985424000000.
  3. signal 1 entered state 1 (`off`) at time 130985418600000.

### `POST /api/signals`

Requires the `update_signals` permission.

Alters the state of a signal.

A typical client might be a subscriber of a camera's built-in motion
detection event stream or of a security system's zone status event stream.
It makes a request on every event or on every 30 second timeout, predicting
that the state will last for a minute. This prediction may be changed later.
Writing to the near future in this way ensures that the UI never displays
`unknown` when the client is actively managing the signal.

Some requests may instead backfill earlier history, such as when a video
analytics client starts up and analyzes all video segments recorded since it
last ran. These will specify beginning and end times.

The request should have an `application/json` body describing the change to
make. It should be a dict with these attributes:

*   `signalIds`: a list of signal ids to change. Must be sorted.
*   `states`: a list (one per `signalIds` entry) of states to set.
*   `start`: the starting time of the change, as a dict of the form
    `{'base': 'epoch', 'rel90k': t}` or `{'base': 'now', 'rel90k': t}`. In
    the `epoch` form, `rel90k` is 90 kHz units since 1970-01-01 00:00:00 UTC.
    In the `now` form, `rel90k` is relative to current time and may be
    negative.
*   `end`: the ending time of the change, in the same form as `start`.

The response will be an `application/json` body dict with the following
attributes:

*   `time90k`: the current time. When the request's `startTime90k` is absent
    and/or its `endBase` is `now`, this is needed to know the effect of the
    earlier request.

Example request sequence:

#### Request 1

The client responsible for reporting live driveway motion has just started. It
observes motion now. It records no history and predicts there will be motion
for the next minute.

Request:

```json
{
  "signalIds": [1],
  "states": [2],
  "start": {"base": "now", "rel90k": 0},
  "end": {"base": "now", "rel90k": 5400000}
}
```

Response:

```json
{
  "time90k": 140067468000000
}
```

#### Request 2

30 seconds later (half the prediction interval), the client still observes
motion. It leaves the prior data alone and predicts the motion will continue.

Request:

```json
{
  "signalIds": [1],
  "states": [2],
  "start": {"base": "epoch", "rel90k": 140067468000000},
  "end": {"base": "now", "rel90k": 5400000}
}
```

Response:

```json
{
  "time90k": 140067470700000
}
```

#### Request 3

5 seconds later, the client observes motion has ended. It leaves the prior
data alone and predicts no more motion.

Request:

```json
{
  "signalIds": [1],
  "states": [2],
  "start": {"base": "now", "rel90k": 0},
  "end": {"base": "now", "rel90k": 5400000}
  }
}
```

Response:

```json
{
  "time90k": 140067471150000
}
```

[media-segment]: https://w3c.github.io/media-source/isobmff-byte-stream-format.html#iso-media-segments
[init-segment]: https://w3c.github.io/media-source/isobmff-byte-stream-format.html#iso-init-segments
[rfc-6381]: https://tools.ietf.org/html/rfc6381
[rfc-6455]: https://tools.ietf.org/html/rfc6455
[multipart-mixed-js]: https://github.com/scottlamb/multipart-mixed-js
