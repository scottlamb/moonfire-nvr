# Moonfire NVR change log

Below are some highlights in each release. For a full description of all
changes, see Git history.

Each release is tagged in Git and on the Docker repository
[`scottlamb/moonfire-nvr`](https://hub.docker.com/r/scottlamb/moonfire-nvr).

Backwards-incompatible database schema changes happen on on major version
upgrades, e.g. `0.6.x` -> `0.7.x`. The config file format and
[API](ref/api.md) currently have no stability guarantees, so they may change
even on minor releases, e.g. `0.7.5` -> `0.7.6`.

## unreleased

*   bump minimum Rust version to 1.64.
*   expect camelCase in `moonfire-nvr.toml` file, for consistency with the JSON
    API. You'll need to adjust your config file when upgrading.
*   use Retina 0.4.4.
    * This version is newly compatible with rtsp-simple-server v0.19.3 and some
      TP-Link cameras. Fixes [#238](https://github.com/scottlamb/moonfire-nvr/issues/238).
    * Fixes problems connecting to cameras that use RTP extensions.
*   expanded API interface for examining and updating users:
    *   `admin_users` permission for operating on arbitrary users.
    *   `GET /users/` endpoint to list users
    *   `POST /users/` endpoint to add a user
    *   `GET /users/<id>` endpoint to examine a user in detail
    *   expanded `PATCH /users/<id>` endpoint, including password and
        permissions.
    *   `DELETE /users/<id>` endpoint to delete a user
*   improved API documentation in [`ref/api.md`](ref/api.md).
*   first draft of a web UI for user administration. Rough edges expected!

## 0.7.5 (2022-05-09)

*   [#219](https://github.com/scottlamb/moonfire-nvr/issues/219): fix
    live stream failing with `ws close: 1006` on URLs with port numbers.
*   build Docker images with link-time optimization.
*   bump minimum Rust version to 1.60.
*   [#224](https://github.com/scottlamb/moonfire-nvr/issues/224): upgrade to
    Retina 0.3.10, improving compatibility with OMNY M5S2A 2812 cameras that
    send invalid `rtptime` values.

## 0.7.4 (2022-04-13)

*   upgrade to Retina 0.3.9, improving camera interop and diagnostics.
    Fixes [#213](https://github.com/scottlamb/moonfire-nvr/issues/213),
    [#209](https://github.com/scottlamb/moonfire-nvr/issues/209).
*   [#217](https://github.com/scottlamb/moonfire-nvr/issues/217): no longer
    drop the connection to the camera when it changes video parameters, instead
    continuing the run seamlessly.
*   [#206](https://github.com/scottlamb/moonfire-nvr/issues/206#issuecomment-1086442543):
    fix `teardown Sender shouldn't be dropped: RecvError(())` errors on shutdown.

## 0.7.3 (2022-03-22)

*   security fix: check the `Origin` header on live stream WebSocket requests
    to avoid cross-site WebSocket hijacking (CSWSH).
*   RTSP connections always use the Retina library rather than FFmpeg.

## 0.7.2 (2022-03-16)

*   introduce a configuration file `/etc/moonfire-nvr.toml`; you will need
    to create one when upgrading.
*   bump minimum Rust version from 1.53 to 1.56.
*   fix [#187](https://github.com/scottlamb/moonfire-nvr/issues/187):
    incompatibility with cameras that (incorrectly) omit the SDP origin line.
*   fix [#182](https://github.com/scottlamb/moonfire-nvr/issues/182): error
    on upgrade from schema 6 to schema 7 when a camera's `onvif_host` is empty.
*   API bugfix: in the `GET /api/` response, include `ext` streams if
    configured.
*   fix [#184](https://github.com/scottlamb/moonfire-nvr/issues/184):
    Moonfire NVR would stop recording on a camera that hit the live555 stale
    file descriptor bug, rather than waiting for the stale session to expire.
*   progress on [#70](https://github.com/scottlamb/moonfire-nvr/issues/184):
    shrink the binary from 154 MiB to 70 MiB by reducing debugging information.

## 0.7.1 (2021-10-27)

*   bugfix: editing a camera from `nvr config` would erroneously clear the
    sample file directory associated with its streams.
*   RTSP transport (TCP or UDP) can be set per-stream from `nvr config`.

## 0.7.0 (2021-10-27)

*   [schema version 7](guide/schema.md#version-7)
*   Changes to the [API](guide/api.md):
    *   Added fields to the `GET /api/` response:
        *   `serverVersion`
    *   Altered fields in the `GET /api/` response:
        *   `session` was moved into a new `user` object, to support providing
            information about the user when authenticating via Unix uid rather
            than session cookie (a planned feature). `session.username` is now
            `user.name`; `session.csrf` is now `user.session.csrf`. `user.id`
            and `user.preferences` have been added.
        *   `signals.source` is now `signals.uuid`. The UUID is now expected to
            be unique, where before only (source, type) was guaranteed to be
            unique.
        *   `camera.config` has been altered and extended. `onvifHost` has
            become `onvifBaseUrl` to allow selecting between `http` and `https`.
        *   `camera.description` was moved to `camera.config.description`.
            (This might have been an oversight; now it's only possible to see
            the description with the `read_camera_configs` permission. This
            field can be re-introduced if desired.)
        *   `stream.config` has been altered and extended. `rtspUrl` has become
            `url` to (in the future) represent a URL for other streaming
            protocols. The `record` boolean was replaced with `mode`, which
            currently may be either absent or the string `record`.
    *   Added `POST /api/users/<id>` for altering a user's UI preferences.

## 0.6.7 (2021-10-20)

*   trim whitespace when detecting time zone by reading `/etc/timezone`.
*   (Retina 0.3.2) better `TEARDOWN` handling with the default
    `--rtsp-library=retina` (see
    [scottlamb/retina#34](https://github.com/scottlamb/retina/34)).
    This means faster recovery after an error when using UDP or when the
    camera's firmware is based on an old live555 release.
*   (Retina 0.3.3) better authentication support with the default
    `--rtsp-library=retina` (see
    [scottlamb/retina#25](https://github.com/scottlamb/retina/25)).

## 0.6.6 (2021-09-23)

*   fix [#146](https://github.com/scottlamb/moonfire-nvr/issues/146): "init
    segment fetch error" when browsers have cached data from `v0.6.4` and
    before.
*   fix [#147](https://github.com/scottlamb/moonfire-nvr/issues/147): confusing
    `nvr init` failures when using very old versions of SQLite.
*   fix [#157](https://github.com/scottlamb/moonfire-nvr/issues/157): broken
    live view when using multi-view and selecting the first listed camera
    then selecting another camera for the upper left grid square.
*   support `--rtsp-transport=udp`, which may work better with cameras that
    use old versions of the live555 library, including many Reolink models.
*   send RTSP `TEARDOWN` requests on UDP or with old live555 versions; wait out
    stale sessions before reconnecting to the same camera. This may improve
    reliability with old live555 versions when using TCP also.
*   improve compatibility with cameras that send non-compliant SDP, including
    models from Geovision and Anpviz.
*   fix [#117](https://github.com/scottlamb/moonfire-nvr/issues/117): honor
    shutdown requests when out of disk space, instead of retrying forever.
*   shut down immediately on a second `SIGINT` or `SIGTERM`. The normal
    "graceful" shutdown will still be slow in some cases, eg when waiting for a
    RTSP UDP session to time out after a `TEARDOWN` failure. This allows the
    impatient to get fast results with ctrl-C when running interactively, rather
    than having to use `SIGKILL` from another terminal.

## 0.6.5 (2021-08-13)

*   UI: improve video aspect ratio handling. Live streams formerly worked
    around a Firefox pixel aspect ratio bug by forcing all videos to 16:9, which
    dramatically distorted 9:16 camera views. Playback didn't have the same
    workaround, so anamorphic videos looked correct on Chrome but slightly
    stretched on Firefox. Now both live streams and playback are fully correct
    on all browsers.
*   UI: better error messages on live view when browser is unsupported,
    `sub` stream is unconfigured, or `sub` stream is not set to record.
*   upgrade to retina v0.1.0, which uses `SET_PARAMETERS` rather than
    `GET_PARAMETERS` as a RTSP keepalive. GW Security cameras would ignored
    the latter, causing Moonfire NVR to drop the connection every minute.

## 0.6.4 (2021-06-28)

*   Default to a new pure-Rust RTSP library, `retina`. If you hit problems, you
    can switch back via `--rtsp-library=ffmpeg`. Please report a bug if this
    helps!
*   Correct the pixel aspect ratio of 9:16 sub streams (eg a standard 16x9
    camera rotated 90 degrees) in the same way as 16:9 sub streams.

## 0.6.3 (2021-03-31)

*   New user interface! Besides a more modern appearance, it has better
    error handling and an experimental live view UI.
*   Compile fix for nightly rust 2021-03-14 and beyond.
*   Fix incorrect `prev_media_duration_90k` calculation. No current impact.
    This field is intended to be used in an upcoming scrub bar UI, and when
    not calculated properly there might be unexpected gaps or overlaps in
    playback.

## 0.6.2 (2021-03-12)

*   Fix panics when a stream's PTS has extreme jumps
    ([#113](https://github.com/scottlamb/moonfire-nvr/issues/113))
*   Improve logging. Console log output is now color-coded. ffmpeg errors
    and panics are now logged in the same way as other messages.
*   Fix an error that could prevent the
    `moonfire-nvr check --delete-orphan-rows` command from actually deleting
    rows.

## 0.6.1 (2021-02-16)

*   Improve the server's error messages on the console and in logs.
*   Switch the UI build from the `yarn` package manager to `npm`.
    This makes Moonfire NVR a bit easier to build from scratch.
*   Extend the `moonfire-nvr check` command to clean up several problems that
    can be caused by filesystem corruption.
*   Set the page size to 16 KiB on `moonfire-nvr init` and
    `moonfire-nvr upgrade`. This improves performance.
*   Fix mangled favicons
    ([#105](https://github.com/scottlamb/moonfire-nvr/issues/105))

## 0.6.0 (2021-01-22)

This is the first tagged version and first Docker image release. I chose the
version number 0.6.0 to match the current schema version 6.
