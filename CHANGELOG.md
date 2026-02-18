# Moonfire NVR change log

Below are some highlights in each release. For a full description of all
changes, see Git history. Each release is tagged in git.

Backwards-incompatible database schema changes happen on on major version
upgrades, e.g. `v0.6.x` -> `v0.7.x`. The config file format and
[API](ref/api.md) currently have no stability guarantees, so they may change
even on minor releases, e.g. `v0.7.5` -> `v0.7.6`.

## unreleased

*   Fix React warnings (and potential malfunction) when opening a recording
    in list view. `FullScreenVideo` was not wrapped in `React.forwardRef`,
    so MUI's `Modal`/`FocusTrap` could not attach a ref to it. Also fix
    an MUI warning about the modal content node not accepting focus, by
    adding `tabIndex={-1}` to `FullScreenVideo`'s root element.
*   Fix frequent intermittent live view failures in Firefox.
    Concurrent `Blob.arrayBuffer()` calls in the WebSocket message handler
    could resolve out of order, delivering fMP4 segments to the SourceBuffer
    with non-monotonic decode timestamps. Fix by processing messages
    sequentially via promise chaining.
    Fixes [#343](https://github.com/scottlamb/moonfire-nvr/issues/343).
*   Use real hyperlinks to issues in live view error alert.
    Fixes [#326](https://github.com/scottlamb/moonfire-nvr/issues/326).
*   Fix 9x16 videos being stretched to the full screen dimensions in
    list view full screen mode. Clean up some references to ancient
    Firefox versions along the way.
*   Test and fix dir pool shutdown with multiple streams.
    `v0.7.26` introduced a busy loop error in this case.
*   Fix debug-mode panics within `moonfire-nvr config` command.
*   Update to [retina 0.4.17](https://github.com/scottlamb/retina/blob/main/CHANGELOG.md#v0417-2026-02-17),
    which improves compatibility with several camera models.

## v0.7.26 (2026-02-17)

*   Major changes to sample file directory IO.
    *   All directory I/O happens on each directory's two-thread pool with threads
        named `dir-<PATH>`. Formerly, I/O could happen on the per-stream writer
        threads, per-directory reader threads, and occasionally on even the
        tokio worker threads.
    *   Stream shutdowns are more responsive now. Before they could wait up to
        30 seconds for a connect/frame timeout; now the shutdown request aborts
        these operations.
    *   Multiple sample file directories are scanned in parallel, speeding
        program startup.
    *   Streamers write only into a recent frames buffer in RAM, which the directory
        pool workers asynchronously write to disk approximately once per GOP, where
        formerly there was a write call per frame.

        In the new model, streamers never fall behind due to HDD I/O. Instead, frames
        are buffered up to a limit. Recordings are aborted if the pool falls too
        far behind.

        This roughly halves CPU usage as thread hand-offs and syscalls are less common.
        It also enables several desirable future changes:

        *    audio recording (which radically increases the number of frames and thus
             would otherwise significantly increase CPU usage).
        *    "monitor mode" (allowing live view without recording to disk).
        *    further CPU reductions for live view (by skipping all SQLite ops and disk I/O).

    *   Streamers never acquire the full SQLite database lock, only a per-stream lock.
        They thus never wait on SQLite operations, except indirectly if the tokio thread
        is performing SQLite work on another task. A future change will move all
        SQLite operations to a dedicated thread to avoid such stalls. This was previously
        not viable because it would cause too many thread hand-offs.

*   bump minimum Rust version to 1.91.

## v0.7.25 (2026-02-14)

*   fix release workflow error

## v0.7.24 (2026-02-14)

(This version was tagged but never released due to an error.)

*   in UI, replace unhelpful `TypeError: Failed to fetch` messages with more descriptive errors such as `POST /api/login failed; see browser console`.
*   update the tested and supported [Node.js](https://nodejs.org/en) versions to 20, 22, 24, or 25. Fix some test failures on 24+.
*   quick fix for `valid timestamp: parameter 'second' with value 102481911520608 is not in the required range of -377705023201..=253402207200 ` panics.
    See [#346](https://github.com/scottlamb/moonfire-nvr/issues/346).
*   abort on panics. This avoids situations in which the binary continues to run in a broken state after mutexes have been "poisoned". It should be less confusing, and when coupled with typical systemd/Docker policies, will cause the system to recover on its own.
*   reduce the amount of debug info in the binary.
*   update Retina from v0.4.14 to v0.4.16, improving camera compatibility.

## v0.7.23 (2025-10-03)

*   update Retina to [v0.4.14](https://github.com/scottlamb/retina/blob/main/CHANGELOG.md#v0414-2025-10-03),
    improving camera compatibility. Fixes [#344](https://github.com/scottlamb/moonfire-nvr/issues/344).
*   bump minimum Rust version to 1.88.

## v0.7.22 (2025-10-03)

(This version was tagged but never released due to an error.)

*   switch from per-layer to global `tracing` filter to avoid missing log lines
    (tokio-rs/tracing#2519)[https://github.com/tokio-rs/tracing/issues/2519]).

## v0.7.21 (2025-04-04)

*   Release with `mimalloc` allocator, which is significantly faster than the memory
    allocator built into `musl`.
*   Eliminate some memory allocations.

## v0.7.20 (2025-01-31)

*   H.265 fixes.

## v0.7.19 (2025-01-28)

*   support recording H.265 ([#33](https://github.com/scottlamb/moonfire-nvr/issues/33)).
    Browser support may vary.
*   bump minimum Rust version to 1.82.
*   improve error message on timeout opening stream.
*   use `jiff` for time manipulations.

## v0.7.18 (2025-01-28)

This release was skipped due to build problems on `armv7-unknown-linux-musleabif`.

## v0.7.17 (2024-09-03)

*   bump minimum Rust version to 1.79.
*   in UI's list view, add a tooltip on the end time which shows why the
    recording ended.
*   fix [#121](https://github.com/scottlamb/moonfire-nvr/issues/121):
    iPhone live view.
*   update to hyper and http version 1.0. In the process, no longer wait for
    pending HTTP requests on shutdown. This just extended the time Moonfire was
    running without streaming.
*   upgrade to Retina 0.4.10, adding support for recording MJPEG video. Note
    major browsers do not support playback of MJPEG videos, however.

## v0.7.16 (2024-05-30)

*   further changes to improve Reolink camera compatibility.

## v0.7.15 (2024-05-26)

*   update Retina to 0.4.8, improving compatibility with some Reolink cameras.
    See [retina#102](https://github.com/scottlamb/retina/issues/102).

## v0.7.14 (2024-04-16)

*   Many UI improvements in [#315](https://github.com/scottlamb/moonfire-nvr/pull/315)
    from [@michioxd](https://github.com/michioxd). See the PR description for
    full details, including screenshots.
    *   dark/light modes
    *   redesigned login dialog
    *   live view: new dual camera layout, more descriptive layout names,
        full screen option, re-open with last layout and camera selection
    *   list view: filter button becomes outlined when enabled
*   Fix [#286](https://github.com/scottlamb/moonfire-nvr/issues/286):
    live view now works on Firefox! Formerly, it'd fail with messages such as
    `Security Error: Content at https://mydomain.com/ may not load data from blob:https://mydomain.com/44abc5dc-750d-48d1-817d-2e6a52445592`.


## v0.7.13 (2024-02-12)

*   seamlessly merge together recordings which have imperceptible changes in
    their `VideoSampleEntry`. Improves
    [#302](https://github.com/scottlamb/moonfire-nvr/issues/302).

## v0.7.12 (2024-01-08)

*   update to Retina 0.4.7, supporting RTSP servers that do not set
    a `Content-Type` in their `DESCRIBE` responses

## v0.7.11 (2023-12-29)

*   upgrade some Rust dependencies. Most notably, Retina 0.4.6 improves camera
    compatibility.
*   update frontend build and tests from no-longer-maintained create-react-app
    to vite and vitest.

## v0.7.10 (2023-11-28)

*   build docker images again
*   upgrade date/time pickers to `@mui/x-date-pickers` v6 beta, improving
    time entry. Fixes
    [#256](https://github.com/scottlamb/moonfire-nvr/issues/256).

## v0.7.9 (2023-10-21)

*  `systemd` integration on Linux
   *   notify `systemd` on starting/stopping. To take advantage of this, you'll
       need to modify your `/etc/systemd/moonfire-nvr.service`. See
       [`guide/install.md`](guide/install.md).
   *   socket activation. See [`ref/config.md`](ref/config.md).

## v0.7.8 (2023-10-18)

*  release as self-contained Linux binaries (for `x86_64`, `aarch64`, and
   `armv8` architectures) rather than Docker images. This minimizes hassle and
   total download size. Along the way, we switched libc from `glibc` to `musl`.
   Please report any problems with the build or instructions!

## v0.7.7 (2023-08-03)

*  fix [#289](https://github.com/scottlamb/moonfire-nvr/issues/289): crash on
   pressing the `Add` button in the sample file directory dialog
*  log to `stderr` again, fixing a regression with the `tracing` change in 0.7.6.
*  experimental (off by default) support for bundling UI files into the
   executable.

## v0.7.6 (2023-07-08)

*   new log formats using `tracing`. This will allow richer context information.
*   bump minimum Rust version to 1.70.
*   expect camelCase in `moonfire-nvr.toml` file, for consistency with the JSON
    API. You'll need to adjust your config file when upgrading.
*   use Retina 0.4.5.
    * This version is newly compatible with rtsp-simple-server v0.19.3 and some
      TP-Link cameras. Fixes [#238](https://github.com/scottlamb/moonfire-nvr/issues/238).
    * Fixes problems connecting to cameras that use RTP extensions.
    * Fixes problems with Longse cameras
      [scottlamb/retina#77](https://github.com/scottlamb/retina/pull/77).
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
*   `moonfire-nvr login --permissions` now accepts the JSON format documented
    in `ref/api.md`, not an undocumented plaintext protobuf format.
*   fix [#257](https://github.com/scottlamb/moonfire-nvr/issues/257):
    Live View: select None Not Possible After Selecting a Camera.
*   get rid of live view's dreaded `ws close: 1006` error altogether. The live
    view WebSocket protocol now conveys errors in a way that allows the
    Javscript UI to see them.
*   fix [#282](https://github.com/scottlamb/moonfire-nvr/issues/282):
    sessions' last use information wasn't getting persisted.
*   improvements to `moonfire-nvr config`,
    thanks to [@sky1e](https://github.com/sky1e).

## v0.7.5 (2022-05-09)

*   [#219](https://github.com/scottlamb/moonfire-nvr/issues/219): fix
    live stream failing with `ws close: 1006` on URLs with port numbers.
*   build Docker images with link-time optimization.
*   bump minimum Rust version to 1.60.
*   [#224](https://github.com/scottlamb/moonfire-nvr/issues/224): upgrade to
    Retina 0.3.10, improving compatibility with OMNY M5S2A 2812 cameras that
    send invalid `rtptime` values.

## v0.7.4 (2022-04-13)

*   upgrade to Retina 0.3.9, improving camera interop and diagnostics.
    Fixes [#213](https://github.com/scottlamb/moonfire-nvr/issues/213),
    [#209](https://github.com/scottlamb/moonfire-nvr/issues/209).
*   [#217](https://github.com/scottlamb/moonfire-nvr/issues/217): no longer
    drop the connection to the camera when it changes video parameters, instead
    continuing the run seamlessly.
*   [#206](https://github.com/scottlamb/moonfire-nvr/issues/206#issuecomment-1086442543):
    fix `teardown Sender shouldn't be dropped: RecvError(())` errors on shutdown.

## v0.7.3 (2022-03-22)

*   security fix: check the `Origin` header on live stream WebSocket requests
    to avoid cross-site WebSocket hijacking (CSWSH).
*   RTSP connections always use the Retina library rather than FFmpeg.

## v0.7.2 (2022-03-16)

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

## v0.7.1 (2021-10-27)

*   bugfix: editing a camera from `nvr config` would erroneously clear the
    sample file directory associated with its streams.
*   RTSP transport (TCP or UDP) can be set per-stream from `nvr config`.

## v0.7.0 (2021-10-27)

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

## v0.6.7 (2021-10-20)

*   trim whitespace when detecting time zone by reading `/etc/timezone`.
*   (Retina 0.3.2) better `TEARDOWN` handling with the default
    `--rtsp-library=retina` (see
    [scottlamb/retina#34](https://github.com/scottlamb/retina/34)).
    This means faster recovery after an error when using UDP or when the
    camera's firmware is based on an old live555 release.
*   (Retina 0.3.3) better authentication support with the default
    `--rtsp-library=retina` (see
    [scottlamb/retina#25](https://github.com/scottlamb/retina/25)).

## v0.6.6 (2021-09-23)

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

## v0.6.5 (2021-08-13)

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

## v0.6.4 (2021-06-28)

*   Default to a new pure-Rust RTSP library, `retina`. If you hit problems, you
    can switch back via `--rtsp-library=ffmpeg`. Please report a bug if this
    helps!
*   Correct the pixel aspect ratio of 9:16 sub streams (eg a standard 16x9
    camera rotated 90 degrees) in the same way as 16:9 sub streams.

## v0.6.3 (2021-03-31)

*   New user interface! Besides a more modern appearance, it has better
    error handling and an experimental live view UI.
*   Compile fix for nightly rust 2021-03-14 and beyond.
*   Fix incorrect `prev_media_duration_90k` calculation. No current impact.
    This field is intended to be used in an upcoming scrub bar UI, and when
    not calculated properly there might be unexpected gaps or overlaps in
    playback.

## v0.6.2 (2021-03-12)

*   Fix panics when a stream's PTS has extreme jumps
    ([#113](https://github.com/scottlamb/moonfire-nvr/issues/113))
*   Improve logging. Console log output is now color-coded. ffmpeg errors
    and panics are now logged in the same way as other messages.
*   Fix an error that could prevent the
    `moonfire-nvr check --delete-orphan-rows` command from actually deleting
    rows.

## v0.6.1 (2021-02-16)

*   Improve the server's error messages on the console and in logs.
*   Switch the UI build from the `yarn` package manager to `npm`.
    This makes Moonfire NVR a bit easier to build from scratch.
*   Extend the `moonfire-nvr check` command to clean up several problems that
    can be caused by filesystem corruption.
*   Set the page size to 16 KiB on `moonfire-nvr init` and
    `moonfire-nvr upgrade`. This improves performance.
*   Fix mangled favicons
    ([#105](https://github.com/scottlamb/moonfire-nvr/issues/105))

## v0.6.0 (2021-01-22)

This is the first tagged version and first Docker image release. I chose the
version number 0.6.0 to match the current schema version 6.
