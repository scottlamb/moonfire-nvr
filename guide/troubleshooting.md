# Troubleshooting <!-- omit in toc -->

Here are some tips for diagnosing various problems with Moonfire NVR. Feel free
to open an [issue](https://github.com/scottlamb/moonfire-nvr/issues) if you
need more help.

* [Viewing Moonfire NVR's logs](#viewing-moonfire-nvrs-logs)
    * [Flushes](#flushes)
    * [Panic errors](#panic-errors)
    * [Slow operations](#slow-operations)
    * [Camera stream errors](#camera-stream-errors)
* [Problems](#problems)
    * [Server errors](#server-errors)
        * [`clock_gettime failed: EPERM: Operation not permitted`](#clock_gettime-failed-eperm-operation-not-permitted)
        * [`Error: pts not monotonically increasing; got 26615520 then 26539470`](#error-pts-not-monotonically-increasing-got-26615520-then-26539470)
        * [Out of disk space](#out-of-disk-space)
        * [Database or filesystem corruption errors](#database-or-filesystem-corruption-errors)
        * [Incorrect timestamps](#incorrect-timestamps)
    * [Configuration interface problems](#configuration-interface-problems)
        * [`moonfire-nvr config` displays garbage](#moonfire-nvr-config-displays-garbage)
    * [Errors in kernel logs](#errors-in-kernel-logs)
        * [UAS errors](#uas-errors)
        * [Filesystem errors](#filesystem-errors)

## Viewing Moonfire NVR's logs

While Moonfire NVR is running, logs will be written to stderr.

*   When running the configuration UI, you typically should redirect stderr
    to a text file to avoid poor interaction between the interactive stdout
    output and the logging. If you use the recommended
    `nvr config 2>debug-log` command, output will be in the `debug-log` file.
*   When running detached through Docker, Docker saves the logs for you.
    Try `nvr logs` or `docker logs moonfire-nvr`.
*   When running through systemd, stderr will be redirected to the journal.
    Try `sudo journalctl --unit moonfire-nvr` to view the logs. You also
    likely want to set `MOONFIRE_FORMAT=systemd` to format logs as
    expected by systemd.

*Note:* Moonfire's log format has recently changed significantly. You may
encounter the older format in the issue tracker or (despite best efforts)
documentation that hasn't been updated.

Logging options are controlled by environment variables:

*   `MOONFIRE_LOG` controls the log level. Its format is similar to the
    `RUST_LOG` variable used by the
    [env-logger](http://rust-lang-nursery.github.io/log/env_logger/) crate.
    `MOONFIRE_LOG=info` is the default.
    `MOONFIRE_LOG=info,moonfire_nvr=debug` gives more detailed logging of the
    `moonfire_nvr` crate itself.
*   `MOONFIRE_FORMAT` selects an output format. It defaults to an output meant
    for human consumption. It can be overridden to either of the following:
    *   `systemd` uses [sd-daemon logging prefixes](https://man7.org/linux/man-pages/man3/sd-daemon.3.html))
    *   `json` outputs one JSON-formatted log message per line, for machine
        consumption.
*   Errors include a backtrace if `RUST_BACKTRACE=1` is set.

If you use Docker, set these via Docker's `--env` argument.

With `MOONFIRE_FORMAT` left unset, log events look as follows:

```text
2023-02-15T22:45:06.999329  INFO                   s-courtyard-sub streamer{stream="courtyard-sub"}: moonfire_nvr::streamer: opening input url=rtsp://192.168.5.112/cam/realmonitor?channel=1&subtype=1&unicast=true&proto=Onvif
```

This example contains the following elements:

*   the timestamp (`2023-02-15T22:45:06.9999329`) in the system's local zone.
*   the log level (`INFO`) is one of `TRACE`, `DEBUG`, `INFO`, `WARN`, or
    `ERROR`.
*   the thread name (`s-courtyard-sub`), see explanation below.
*   the "spans" (`streamer{stream="courtyard-sub"}`), which contain
    context information for a group of messages. In this case there is a single
    span `streamer` with a single field `stream`. There can be multiple
    spans; they are listed starting from the root. Each may have fields.
*   the target (`moonfire_nvr::streamer`), which generally corresponds to a Rust
    module name.
*   the log message (`opening input`), a human-readable string
*   event fields (`url=...`)

Moonfire NVR names a few important thread types as follows:

*   `main`: during `moonfire-nvr run`, the main thread does initial setup then
    just waits for the other threads. In other subcommands, it does everything.
*   `s-CAMERA-TYPE` (one per stream, where `TYPE` is `main`, `sub`, or `ext`):
    these threads write video to disk.
*   `sync-DIR_ID` (one per sample file directory): These threads call `fsync` to
*   commit sample files to disk, delete old sample files, and flush the
    database.
*   `r-DIR_ID` (one per sample file directory): These threads read sample files
    from disk for serving `.mp4` files.
*   `tokio-runtime-worker` (one per core, unless overridden with
    `--worker-threads`): these threads handle HTTP requests and read video
    data from cameras via RTSP.

Below are some interesting log lines you may encounter.

### Flushes

During normal operation, Moonfire NVR will periodically flush changes to its
SQLite3 database. Every flush is logged, as in the following info message:

```
2021-03-08T23:14:18.388000 sync-2 syncer{path=/media/14tb/sample}:flush{flush_count=2 reason="120 sec after start of 1 minute 14 seconds courtyard-main recording 3/1842086"}: moonfire_db::db: flush complete:
/media/6tb/sample: added 98M 864K 842B in 8 recordings (4/1839795, 7/1503516, 6/1853939, 1/1838087, 2/1852096, 12/1516945, 8/1514942, 10/1506111), deleted 111M 435K 587B in 5 (4/1801170, 4/1801171, 6/1799708, 1/1801528, 2/1815572), GCed 9 recordings (6/1799707, 7/1376577, 4/1801168, 1/1801527, 4/1801167, 4/1801169, 10/1243252, 2/1815571, 12/1418785).
/media/14tb/sample: added 8M 364K 643B in 3 recordings (3/1842086, 9/1505359, 11/1516695), deleted 0B in 0 (), GCed 0 recordings ().
```

This log message is packed with debugging information:

*   the date and time: `2021-03-08T23:14:18.388`.
*   the name of the thread that prompted the flush: `sync-2`.
*   a flush count: `3810`. This is handy for checking how often Moonfire NVR
    is flushing.
*   a reason for the flush: `120 sec after start of 1 minute 14 seconds courtyard-main recording 3/1842086`.
    This was a regular periodic flush at the `flush_if_sec` for the stream,
    as described in [install.md](install.md). `3/1842086` is an identifier for
    the recording, in the form `stream_id/recording_id`. It corresponds to the
    file `/media/14tb/sample/00000003001c1ba6`. On-disk files are named by
    a fixed eight hexadecimal digits for the stream id and eight hexadecimal
    digits for the recording id. You can convert with `printf`:
    ```console
    $ printf '%08x%08x\n' 3 1842086
    00000003001c1ba6
    ```
*   For each affected sample file directory (`/media/6tb/sample` and
    `/media/14tb/sample`), a line showing the exact changes included in the
    flush. There are three kinds of changes:

    *   added recordings–these files are already fully written in the sample
        file directory and now are being added to the database.
    *   deleted recordings–these are being removed from the database's
        `recording` table (and added to the `garbage` table) in preparation
        for being deleted from the sample file directory. They can no longer
        be accessed after this flush.
    *   GCed (garbage-collected) recordings—these have been fully removed from
        disk and no longer will be referenced in the database at all.

    You can learn more about these in the "Lifecycle of a recording" section
    of the [recording schema design document](../design/schema.md).

    For added and deleted recordings, the line includes sizes in bytes
    (`98M 864K 842B` represents 10,3646,026 bytes, or about 99 MiB), numbers
    of recordings, and the IDs of each recording. For GCed recordings, the
    sizes are omitted (as this information is not stored).

### Panic errors

Errors like the one below indicate a serious bug in Moonfire NVR. Please
file a bug if you see one. It's helpful to set the `RUST_BACKTRACE`
environment variable to include more information.

```
2021-03-04T11:09:29.230291 ERROR s-peck_west-main streamer{stream="peck_west-main"}: panic: should always be an unindexed sample location=src/moonfire-nvr/server/db/writer.rs:750:54 backtrace=...
```

In this case, a stream thread (one starting with `s-`) panicked. That stream
won't record again until Moonfire NVR is restarted.

### Slow operations

Warnings like the following indicate that some operation took more than 1
second to perform. `PT2.070715796S` means about 2 seconds.

It's normal to see these warnings on startup and occasionally while running.
Frequent occurrences may indicate a performance problem.

```
2020-11-29T12:01:21.128725 WARN s-driveway-main streamer{stream="driveway-main"}: moonfire_base::clock: opening rtsp://admin:redacted@192.168.5.108/cam/realmonitor?channel=1&subtype=0&unicast=true&proto=Onvif took PT2.070715796S!
2020-11-29T12:32:15.870658 WARN s-west_side-sub streamer{stream="west_side-sub"}: moonfire_base::clock: getting next packet took PT10.158121387S!
2020-12-28T12:09:29.050464 WARN s-back_east-sub streamer{stream="s-back_east-sub"}: moonfire_base::clock: database lock acquisition took PT8.122452
2020-12-28T21:22:32.012811 WARN main moonfire_base::clock: database operation took PT39.526386958S!
2020-12-28T21:27:11.402259 WARN s-driveway-sub streamer{stream="s-driveway-sub"}: moonfire_base::clock: writing 37 bytes took PT20.701894190S!
```

### Camera stream errors

Warnings like the following indicate that a camera stream was lost due to some
error and Moonfire NVR will try reconnecting shortly. `Stream ended` might
happen when the camera is rebooting or if Moonfire is not consuming packets
quickly enough. In the latter case, you'll likely see a
`getting next packet took PT...S!` message as described above.

```
2021-03-09T00:28:55.527078 WARN s-courtyard-sub streamer{stream="courtyard-sub"}: moonfire_nvr::streamer: sleeping for PT1S after error: Stream ended
(set environment variable RUST_BACKTRACE=1 to see backtraces)
```

## Problems

### Server errors

#### `clock_gettime failed: EPERM: Operation not permitted`

If commands fail with an error like the following, you're likely running
Docker with an overly restrictive `seccomp` setup. [This stackoverflow
answer](https://askubuntu.com/questions/1263284/apt-update-throws-signature-error-in-ubuntu-20-04-container-on-arm/1264921#1264921) describes the
problem in more detail. The simplest solution is to add
`--security-opt=seccomp:unconfined` to your Docker commandline.
If you are using the recommended `/usr/local/bin/nvr` wrapper script,
add this option to the `common_docker_run_args` section.

```console
$ sudo docker run --rm -it moonfire-nvr:latest
clock_gettime failed: EPERM: Operation not permitted

This indicates a broken environment. See the troubleshooting guide.
```

#### `Error: pts not monotonically increasing; got 26615520 then 26539470`

If your streams cut out and you see error messages like this one in Moonfire
NVR logs, it might mean that your camera outputs [B
frames](https://en.wikipedia.org/wiki/Video_compression_picture_types#Bi-directional_predicted_.28B.29_frames.2Fslices_.28macroblocks.29).
If you believe this is the case, file a feature request; Moonfire NVR
currently doesn't support B frames. You may be able to configure your camera
to disable B frames in the meantime.

#### Out of disk space

If Moonfire NVR runs out of disk space on a sample file directory, recording
will be stuck and you'll see log messages like the following:

```
2021-04-01T11:21:07.365 WARN s-driveway-main streamer{stream="s-driveway-main"}: moonfire_base::clock: sleeping for PT1S after error: No space left on device (os error 28)
```

If something else used more disk space on the filesystem than planned, just
clean up the excess files. Moonfire NVR will start working again immediately.

If Moonfire NVR's own files are too large, follow this procedure:

1.  Shut it down.
    ```console
    $ sudo killall moonfire-nvr
    ```
2.  Reconfigure it use less disk space. See [Completing configuration through
    the UI](install.md#completing-configuration-through-the-ui) in the
    installation guide. Pay attention to the note about slack space.
3.  Start Moonfire NVR again. It will clean up the excess disk files on
    startup and should run properly.

#### Database or filesystem corruption errors

It's helpful to check out your system's overall health when diagnosing
this kind of problem with Moonfire NVR.

1.  Look at your kernel logs. On most Linux systems, you can browse them via
    `journalctl`, `dmesg`, or `less /var/log/messages`. See [Errors in kernel
    logs](#errors-in-kernel-logs) below for some common problems.
2.  Use [`smartctl`](https://linuxconfig.org/how-to-check-an-hard-drive-health-from-the-command-line-using-smartctl) to
    look at SMART ("Self-Monitoring, Analysis and Reporting Technology System
    (SMART)") attributes on your flash and hard drives. Backblaze
    [reports](https://www.backblaze.com/blog/what-smart-stats-indicate-hard-drive-failures/)
    that the following SMART attributes are most predictive of drive failure:
    *   SMART 5: Reallocated Sectors Count
    *   SMART 187: Reported Uncorrectable Errors
    *   SMART 188: Command Timeout
    *   SMART 197: Current Pending Sector Count
    *   SMART 198: Uncorrectable Sector Count
    If the RAW value for any of these attributes is non-zero, it's likely
    your problem is due to hardware.
3.  Use `smartctl` to run a self-test on your flash and hard drives.
4.  Run `fsck` on your filesystems.

    Your root filesystem is best checked on startup, before it's mounted as
    read-write. On most Linux systems, you can force `fsck` to run on next
    startup via the `fsck.mode=force` kernel parameter, as documented
    [here](https://www.freedesktop.org/software/systemd/man/systemd-fsck@.service.html).

    If you have hard drives dedicated to Moonfire NVR, you can also shut down
    Moonfire NVR, unmount the filesystem, and run `fsck` on them without
    rebooting.

After the system as a whole is verified healthy, run `moonfire-nvr check` while
Moonfire NVR is stopped to verify integrity of the SQLite database and sample
file directories.

#### Incorrect timestamps

Moonfire NVR uses the system clock when a run of recordings starts to determine
the run's initial timestamp. If the system clock is stepped after the run
starts, Moonfire NVR will keep using timestamps based on the old (usually
incorrect) setting.

This is most noticeable on the Raspberry Pi or other cheap SBCs which don't
come with a battery-backed real-time clock (RTC). Instead, they save the
current time periodically and restore it on bootup. Their clocks often are a
few hours behind on startup following a power outage. You may notice in
`journalctl` logs messages similar to the following when the clock is fixed:

```
Aug 14 21:05:51 moonfire moonfire-nvr[710]: Aug 14 21:05:51.538 INFO reserved 590d892d-b2e8-4e6c-9e1b-c4418d0abd69
Aug 14 22:37:39 moonfire systemd[1]: Time has been changed
Aug 14 22:38:48 moonfire moonfire-nvr[710]: Aug 14 22:38:48.965 INFO Committing extra transaction because there's no cached uuid
```

Note the 1.5-hour gap between messages; this is roughly how much the clock was
adjusted.

The exact message may differ based on your Linux distribution and message;
here's another variation:

```
Jul 13 10:05:52 pi4 systemd-timesyncd[340]: Synchronized to time server for the first time [2600:3c00::e:d0bb]:123 (2.debian.pool.ntp.org).
```

Here's what you can do:

*   *recover*: restart Moonfire NVR to pick up the new timestamp.
*   *prevent*: add a RTC module or fresh battery so your clock is correct
    at boot time. There's a
    [guide](https://github.com/scottlamb/moonfire-nvr/wiki/System-setup#realtime-clock-on-raspberry-pi)
    on the wiki.

Currently Moonfire NVR doesn't have any logic to detect this happening or
mechanism to fix old timestamps after the fact. Ideas and help welcome; see
[issue #9](https://github.com/scottlamb/moonfire-nvr/issues/9).

### Configuration interface problems

#### `moonfire-nvr config` displays garbage

This happens if you're not using the premade Docker containers and have
configured your machine is configured to a non-UTF-8 locale, due to
gyscos/Cursive#13. As a workaround, try setting the environment variable
`LC_ALL=C.UTF-8`.

### Errors in kernel logs

#### UAS errors

Some cheap USB SATA adapters don't appear to work reliably in UAS mode under
Linux. If you see errors like the following, try [disabling
UAS](https://github.com/scottlamb/moonfire-nvr/wiki/System-setup#disable-uas).
Unfortunately your filesystem is likely to have corruption, so after disabling UAS,
run a `fsck` and then `moonfire-nvr check` to try recovering.

```
Sep 22 17:26:01 nuc kernel: sd 4:0:0:1: [sdb] tag#2 uas_eh_abort_handler 0 uas-tag 3 inflight: CMD OUT
Sep 22 17:26:01 nuc kernel: sd 4:0:0:1: [sdb] tag#2 CDB: Write(16) 8a 00 00 00 00 01 4d b4 c4 00 00 00 03 b0 00 00
```

#### Filesystem errors

Errors that mention `EXT4-fs` (or your filesystem of choice) likely indicate
filesystem corruption. Run `fsck` to fix as described above. Once the
corruption is addressed, use `moonfire-nvr check` to survey the damage to
your database.

```
Jan 28 07:26:27 nuc kernel: EXT4-fs (sdc1): error count since last fsck: 12
Jan 28 07:26:27 nuc kernel: EXT4-fs (sdc1): initial error at time 1576998292: ext4_validate_block_bitmap:376
Jan 28 07:26:27 nuc kernel: EXT4-fs (sdc1): last error at time 1579640202: ext4_validate_block_bitmap:376
...
Feb 13 04:48:43 nuc kernel: EXT4-fs error (device sdc1): ext4_validate_block_bitmap:376: comm kworker/u8:2: bg 57266: bad block bitmap checksum
Feb 13 04:48:43 nuc kernel: EXT4-fs (sdc1): Delayed block allocation failed for inode 7334278 at logical offset 0 with max blocks 11 with error 74
Feb 13 04:48:43 nuc kernel: EXT4-fs (sdc1): This should not happen!! Data will be lost
```
