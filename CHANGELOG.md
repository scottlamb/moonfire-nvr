# Moonfire NVR change log

Below are some highlights in each release. For a full description of all
changes, see Git history.

Each release is tagged in Git and on the Docker repository
[`scottlamb/moonfire-nvr`](https://hub.docker.com/r/scottlamb/moonfire-nvr).

## `v0.6.3` (in progress)

*   Compile fix for nightly rust 2021-03-14 and beyond.

## `v0.6.2`

*   Fix panics when a stream's PTS has extreme jumps
    ([#113](https://github.com/scottlamb/moonfire-nvr/issues/113))
*   Improve logging. Console log output is now color-coded. ffmpeg errors
    and panics are now logged in the same way as other messages.
*   Fix an error that could prevent the
    `moonfire-nvr check --delete-orphan-rows` command from actually deleting
    rows.

## `v0.6.1`

*   Improve the server's error messages on the console and in logs.
*   Switch the UI build from the `yarn` package manager to `npm`.
    This makes Moonfire NVR a bit easier to build from scratch.
*   Extend the `moonfire-nvr check` command to clean up several problems that
    can be caused by filesystem corruption.
*   Set the page size to 16 KiB on `moonfire-nvr init` and
    `moonfire-nvr upgrade`. This improves performance.
*   Fix mangled favicons
    ([#105](https://github.com/scottlamb/moonfire-nvr/issues/105))

## `v0.6.0`

This is the first tagged version and first Docker image release. I chose the
version number 0.6.0 to match the current schema version 6.
