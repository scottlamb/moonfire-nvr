# Moonfire NVR API

Status: **current**.

## Objective

Allow a JavaScript-based web interface to list cameras and view recordings.

In the future, this is likely to be expanded:

*   configuration support
*   commandline tool over a UNIX-domain socket
    (at least for bootstrapping web authentication)
*   mobile interface

## Detailed design

All requests for JSON data should be sent with the header
`Accept: application/json` (exactly).

### `/api/login`

A `POST` request on this URL should have an `application/x-www-form-urlencoded`
body containing `username` and `password` parameters.

On successful authentication, the server will return an HTTP 204 (no content)
with a `Set-Cookie` header for the `s` cookie, which is an opaque, HttpOnly
(unavailable to Javascript) session identifier.

If authentication or authorization fails, the server will return a HTTP 403
(forbidden) response. Currently the body will be a `text/plain` error message;
future versions will likely be more sophisticated.

### `/api/logout`

A `POST` request on this URL should have an `application/x-www-form-urlencoded`
body containing a `csrf` parameter copied from the `session.csrf` of the
top-level API request.

On success, returns an HTTP 204 (no content) responses. On failure, returns a
4xx response with `text/plain` error message.

### `/api/`

A `GET` request on this URL returns basic information about the server,
including all cameras. Valid request parameters:

*   `days`: a boolean indicating if the days parameter described below
    should be included.

Example request URI:

```
/api/?days=true
```

The `application/json` response will have a dict as follows:

*   `timeZoneName`: the name of the IANA time zone the server is using
    to divide recordings into days as described further below.
*   `cameras`: a list of cameras. Each is a dict as follows:
    *   `uuid`: in text format
    *   `shortName`: a short name (typically one or two words)
    *   `description`: a longer description (typically a phrase or paragraph)
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
        *   `days`: object representing calendar days (in the server's time
            zone) with non-zero total duration of recordings for that day. The
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
      "streams": {
        "main": {
          "retainBytes": 536870912000,
          "minStartTime90k": 130888729442361,
          "maxEndTime90k": 130985466591817,
          "totalDuration90k": 96736169725,
          "totalSampleFileBytes": 446774393937,
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
  "session": {
    "username": "slamb",
    "csrf": "2DivvlnKUQ9JD4ao6YACBJm8XK4bFmOc",
  }
}
```

### `/api/cameras/<uuid>/`

A GET returns information for the camera with the given URL. The information

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

### `/api/cameras/<uuid>/<stream>/recordings`

A GET returns information about recordings, in descending order.

Valid request parameters:

*   `startTime90k` and and `endTime90k` limit the data returned to only
    recordings which overlap with the given half-open interval. Either or both
    may be absent; they default to the beginning and end of time, respectively.
*   `split90k` causes long runs of recordings to be split at the next
    convenient boundary after the given duration.
*   TODO(slamb): `continue` to support paging. (If data is too large, the
    server should return a `continue` key which is expected to be returned on
    following requests.)

TODO(slamb): once we support annotations, should they be included in the same
URI or as a separate `/annotations`?

In the property `recordings`, returns a list of recordings in arbitrary order.
Each recording object has the following properties:

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
*   `startTime90k`: the start time of the given recording. Note this may be
    less than the requested `startTime90k` if this recording was ongoing
    at the requested time.
*   `endTime90k`: the end time of the given recording. Note this may be
    greater than the requested `endTime90k` if this recording was ongoing at
    the requested time.
*   `sampleFileBytes`
*   `videoSampleEntrySha1`
*   `videoSampleEntryWidth`
*   `videoSampleEntryHeight`
*   `videoSamples`: the number of samples (aka frames) of video in this
    recording.

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
      "videoSampleEntrySha1": "81710c9c51a02cc95439caa8dd3bc12b77ffe767",
      "videoSampleEntryWidth": 1280,
      "videoSampleEntryHeight": 720,
    },
    {
      "endTime90k": 130985461191810,
      ...
    },
    ...
  ],
  "continue": "<opaque blob>",
}
```

### `/api/cameras/<uuid>/<stream>/view.mp4`

A GET returns a `.mp4` file, with an etag and support for range requests. The
MIME type will be `video/mp4`, with a `codecs` parameter as specified in [RFC
6381][rfc-6381].

Expected query parameters:

*   `s` (one or more): a string of the form
    `START_ID[-END_ID][@OPEN_ID][.[REL_START_TIME]-[REL_END_TIME]]`. This
    specifies recording segments to include. The produced `.mp4` file will be a
    concatenation of the segments indicated by all `s` parameters.  The ids to
    retrieve are as returned by the `/recordings` URL. The open id is optional
    and will be enforced if present; it's recommended for disambiguation when
    the requested range includes uncommitted recordings. The optional start and
    end times are in 90k units and relative to the start of the first specified
    id. These can be used to clip the returned segments. Note they can be used
    to skip over some ids entirely; this is allowed so that the caller doesn't
    need to know the start time of each interior id. If there is no key frame
    at the desired relative start time, frames back to the last key frame will
    be included in the returned data, and an edit list will instruct the
    viewer to skip to the desired start time.
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

TODO: error behavior on missing segment. It should be a 404, likely with an
`application/json` body describing what portion if any (still) exists.

### `/api/cameras/<uuid>/<stream>/view.mp4.txt`

A GET returns a `text/plain` debugging string for the `.mp4` generated by the
same URL minus the `.txt` suffix.

### `/api/cameras/<uuid>/<stream>/view.m4s`

A GET returns a `.mp4` suitable for use as a [HTML5 Media Source Extensions
media segment][media-segment]. The MIME type will be `video/mp4`, with a
`codecs` parameter as specified in [RFC 6381][rfc-6381].

Expected query parameters:

*   `s` (one or more): as with the `.mp4` URL, except that media segments
    can't contain edit lists so none will be generated. TODO: maybe add a
    `Leading-Time:` header to indicate how many leading 90,000ths of a second
    are present, so that the caller can trim it in some other way.

It's recommended that each `.m4s` retrieval be for at most one Moonfire NVR
recording segment for several reasons:

*   The Media Source Extension API appears structured for adding a complete
    segment at a time. Large media segments thus impose significant latency on
    seeking.
*   There is currently a hard limit of 4 GiB of data because the `.m4s` uses a
    single `moof` followed by a single `mdat`; the former references the
    latter with 32-bit offsets.
*   There's currently no way to generate an initialization segment for more
    than one video sample entry, so a `.m4s` that uses more than one video
    sample entry can't be used.

### `/api/cameras/<uuid>/<stream>/view.m4s.txt`

A GET returns a `text/plain` debugging string for the `.mp4` generated by the
same URL minus the `.txt` suffix.

### `/api/init/<sha1>.mp4`

A GET returns a `.mp4` suitable for use as a [HTML5 Media Source Extensions
initialization segment][init-segment]. The MIME type will be `video/mp4`, with
a `codecs` parameter as specified in [RFC 6381][rfc-6381].

### `/api/init/<sha1>.mp4.txt`

A GET returns a `text/plain` debugging string for the `.mp4` generated by the
same URL minus the `.txt` suffix.

[media-segment]: https://w3c.github.io/media-source/isobmff-byte-stream-format.html#iso-media-segments
[init-segment]: https://w3c.github.io/media-source/isobmff-byte-stream-format.html#iso-init-segments
[rfc-6381]: https://tools.ietf.org/html/rfc6381
