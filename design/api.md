# Moonfire NVR API

Status: **unstable**. This is an early draft; the API may change without
warning.

## Objective

Allow a JavaScript-based web interface to list cameras and view recordings.

In the future, this is likely to be expanded:

*   configuration support
*   commandline tool over a UNIX-domain socket
    (at least for bootstrapping web authentication)
*   mobile interface

## Detailed design

All requests for JSON data should be sent with the header `Accept:
application/json` (exactly). Without this header, replies will generally be in
HTML rather than JSON.

TODO(slamb): authentication.

### `/cameras`

A `GET` request on this URL returns basic information about all cameras. The
`application/json` response will have a top-level `cameras` with a list of
attributes about each camera:

*   `uuid`: in text format
*   `short\_name`: a short name (typically one or two words)
*   `description`: a longer description (typically a phrase or paragraph)
*   `retain\_bytes`: the configured total number of bytes of completed
    recordings to retain.
*   `min\_start\_time\_90k`: the start time of the earliest recording for this
    camera, in 90kHz units since 1970-01-01 00:00:00 UTC.
*   `max\_end\_time\_90k`: the end time of the latest recording for this
    camera, in 90kHz units since 1970-01-01 00:00:00 UTC.
*   `total\_duration\_90k`: the total duration recorded, in 90 kHz units.
    This is no greater than `max\_end\_time\_90k - max\_start\_time\_90k`; it
    will be lesser if there are gaps in the recorded data.
*   `total\_sample\_file\_bytes`: the total number of bytes of sample data (the
    `mdat` portion of a `.mp4` file).

Example response:

```json
{
  "cameras": [
    {
      "uuid": "fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe",
      "short_name": "driveway",
      "description": "Hikvision DS-2CD2032 overlooking the driveway from east",
      "retain_bytes": 536870912000,
      "min_start_time_90k": 130888729442361,
      "max_end_time_90k": 130985466591817
      "total_duration_90k": 96736169725,
      "total_sample_file_bytes": 446774393937,
    },
    ...
  ],
}
```

### `/cameras/<uuid>`

A GET returns information for the camera with the given URL. The information
returned is a superset of that returned by the camera list.

TODO(slamb): this should likely return a list of calendar days with data in the
server's time zone, along with the associated `start\_time\_90k` and
`end\_time\_90k`. The server will calculate this on startup and maintain it
as recordings are updated.

### `/camera/<uuid>/recordings`

A GET returns information about recordings, in descending order.

TODO(slamb): once we support annotations, should they be included in the same
URI or as a separate `/annotations`?

TODO(slamb): this should support paging. The client can limit the range via
the URI parameters `start\_time\_90k` and `end\_time\_90k`. If the range is
too large, the server will return some fraction of the data along with a
continuation key to pass in for the next request.

TODO(slamb): There might be some irregularity in the order if there are
overlapping recordings (such as if the server's clock jumped while running)
but I haven't thought about the details. In general, I'm not really sure how
to handle this case, other than ideally to keep recording stuff no matter what
and present some UI to help the user to fix it after the
fact.

In the property `recordings`, returns a list of recordings. Each recording
object has the following properties:

*   `start\_time\_90k`
*   `end\_time\_90k`
*   `sample\_file\_bytes`
*   `video\_sample\_entry\_sha1`
*   `video\_sample\_entry\_width`
*   `video\_sample\_entry\_height`

TODO(slamb): consider ways to reduce the data size; this is in theory quite
compressible but I'm not sure how effective gzip will be without some tweaks.
One simple approach would be to just combine some adjacent list entries if
one's start matches the other's end exactly and the `video\_sample\_entry\_*`
parameters are the same. So you might get one entry that represents 2 hours of
video instead of 120 entries representing a minute each.

Example request URI (with added whitespace between parameters):

```
/camera/fd20f7a2-9d69-4cb3-94ed-d51a20c3edfe/recordings
    ?start_time_90k=130888729442361
    &end_time_90k=130985466591817
```

Example response:

```json
{
  "recordings": [
    {
      "end_time_90k": 130985466591817,
      "start_time_90k": 130985461191810,
      "sample_file_bytes": 8405564,
      "video_sample_entry_sha1": "81710c9c51a02cc95439caa8dd3bc12b77ffe767",
      "video_sample_entry_width": 1280,
      "video_sample_entry_height": 720,
    },
    {
      "end_time_90k": 130985461191810,
      ...
    },
    ...
  ],
  "continuation_key": "<opaque blob>",
}
```

### `/camera/<uuid>/view.mp4`

A GET returns a .mp4 file, with an etag and support for range requests.

Expected query parameters:

*   `start\_time\_90k`
*   `end\_time\_90k`
*   `ts`: should be set to `true` to request a subtitle track be added with
    human-readable recording timestamps.
*   TODO(slamb): possibly `overlap` to indicate what to do about segments of
    recording with overlapping wall times. Values might include:
    *   `error` (return an HTTP error)
    *   `include_all` (include all, in order of the recording ids)
    *   `include_latest` (include only the latest by recording id for a
        particular segment of time)
*   TODO(slamb): gaps allowed or not? maybe a parameter for this also?
*   TODO(slamb): parameter to indicate if a caption track should be included
    with timestamps?
