# Troubleshooting

Here are some tips for diagnosing various problems with Moonfire NVR. Feel free
to open an [issue](https://github.com/scottlamb/moonfire-nvr/issues) if you
need more help.

## Viewing Moonfire NVR's logs

While Moonfire NVR is running, logs will be written to stderr.

   * When running the configuration UI, you typically should redirect stderr
     to a text file to avoid poor interaction between the interactive stdout
     output and the logging. If you use the recommended
     `nvr config 2>debug-log` command, output will be in the `debug-log` file.
   * When running detached through Docker, Docker saves the logs for you.
     Try `nvr logs` or `docker logs moonfire-nvr`.
   * When running through systemd, stderr will be redirected to the journal.
     Try `sudo journalctl --unit moonfire-nvr` to view the logs. You also
     likely want to set `MOONFIRE_FORMAT=google-systemd` to format logs as
     expected by systemd.

Logging options are controlled by environment variables:

   * `MOONFIRE_LOG` controls the log level. Its format is similar to the
     `RUST_LOG` variable used by the
     [env-logger](http://rust-lang-nursery.github.io/log/env_logger/) crate.
     `MOONFIRE_LOG=info` is the default.
     `MOONFIRE_LOG=info,moonfire_nvr=debug` gives more detailed logging of the
     `moonfire_nvr` crate itself.
   * `MOONFIRE_FORMAT` selects the output format. The two options currently
     accepted are `google` (the default, like the Google
     [glog](https://github.com/google/glog) package) and `google-systemd` (a
     variation for better systemd compatibility).
   * Errors include a backtrace if `RUST_BACKTRACE=1` is set.

If you use Docker, set these via Docker's `--env` argument.

## Problems

### `Error: pts not monotonically increasing; got 26615520 then 26539470`

If your streams cut out and you see error messages like this one in Moonfire
NVR logs, it might mean that your camera outputs [B
frames](https://en.wikipedia.org/wiki/Video_compression_picture_types#Bi-directional_predicted_.28B.29_frames.2Fslices_.28macroblocks.29).
If you believe this is the case, file a feature request; Moonfire NVR
currently doesn't support B frames. You may be able to configure your camera
to disable B frames in the meantime.

### `moonfire-nvr config` displays garbage

This happens if your machine is configured to a non-UTF-8 locale, due to
gyscos/Cursive#13. As a workaround, try setting the environment variable
`LC_ALL=C.UTF-8`. This should automatically be set with the Docker container.

### Moonfire NVR reports problems with the database or filesystem

It's helpful to check out your system's overall health when diagnosing
problems with Moonfire NVR.

1.  Look at your kernel logs. On most Linux systems, you can browse them via
    `journalctl`, `dmesg`, or `less /var/log/messages`. See [Errors in kernel
    logs](#error) below for some common problems.
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

### <a name="kernel-errors"></a> Errors in kernel logs

#### UAS errors

Some cheap USB SATA adapters don't appear to work reliably in UAS mode under
Linux. If you see errors like the following, try [disabling
UAS](https://unix.stackexchange.com/questions/239782/connection-problem-with-usb3-external-storage-on-linux-uas-driver-problem).
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
