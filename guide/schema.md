# Moonfire NVR Schema Guide

This document has notes about the Moonfire NVR storage schema. As described in
[README.md](../README.md), this consists of two kinds of state:

   * a SQLite database, typically <1 GiB. It should be stored on flash if
     available.
   * the "sample file directory", which holds the actual samples/frames of
     H.264 video. This should be quite large and typically is stored on a hard
     drive.

## Upgrading

The database schema includes a version number to quickly identify if a
the database is compatible with a particular version of the software. Some
software upgrades will require you to upgrade the database.

Note that in general upgrades are one-way and backward-incompatible. That is,
you can't downgrade the database to the old version, and you can't run the old
software on the new database. To minimize the corresponding risk, you should
save a backup of the old SQLite database and verify the new software works in
read-only mode prior to deleting the old database.

### Procedure

First ensure there is sufficient space available for four copies of the
SQLite database:

   * copy 1: the copy to upgrade
   * copy 2: a backup you manually create so that you can restore if you
     discover a problem while running the new software against the upgraded
     database in read-only mode. If disk space is tight, you can save this
     to a different filesystem than the primary copy.
   * copies 3 and 4: internal copies made and destroyed by Moonfire NVR and
     SQLite during the upgrade:

        * during earlier steps, possibly duplicate copies of tables, which
          may occupy space both in the main database and the journal
        * during the final vacuum step, a complete database copy

     If disk space is tight, and you are _very careful_, you can skip these
     copies with the `--preset-journal=off --no-vacuum` arguments to
     the updater. If you aren't confident in your ability to do this, *don't
     do it*. If you are confident, take additional safety precautions anyway:

        * double-check you have the full backup described above. Without the
          journal any problems during the upgrade will corrupt your database
          and you will need to restore.
        * ensure you re-enable journalling via `pragma journal_mode = wal;`
          before using the upgraded database, or any problems after the
          upgrade will corrupt your database. The upgrade procedure should do
          this automatically, but you will want to verify by hand that you are
          no longer in the dangerous mode.

Next ensure Moonfire NVR is not running and does not automatically restart if
the system is rebooted during the upgrade. If you followed the Docker
instructions, you can do this as follows:

    $ nvr stop

Then back up your SQLite database. If you are using the default path, you can
do so as follows:

    $ sudo -u moonfire-nvr cp /var/lib/moonfire-nvr/db/db{,.pre-upgrade}

By default, the upgrade command will reset the SQLite `journal_mode` to
`delete` prior to the upgrade. This works around a problem with
`journal_mode = wal` in older SQLite versions, as documented in [the SQLite
manual for write-ahead logging](https://www.sqlite.org/wal.html):

> WAL works best with smaller transactions. WAL does not work well for very
> large transactions. For transactions larger than about 100 megabytes,
> traditional rollback journal modes will likely be faster. For transactions
> in excess of a gigabyte, WAL mode may fail with an I/O or disk-full error.
> It is recommended that one of the rollback journal modes be used for
> transactions larger than a few dozen megabytes. Beginning with version
> 3.11.0 (2016-02-15), WAL mode works as efficiently with large transactions
> as does rollback mode.

Run the upgrade procedure using the new software binary.

```
$ nvr pull     # updates the docker image to the latest binary
$ nvr upgrade  # runs the upgrade
```

As a rule of thumb, on a Raspberry Pi 4 with a 1 GiB database, an upgrade might
take about four minutes for each schema version and for the final vacuum.

Next, you can run the system in read-only mode, although you'll find this only
works in the "insecure" setup. (Authorization requires writing the database.)

```
$ nvr rm
$ nvr run --read-only
```

Go to the web interface and ensure the system is operating correctly. If
you detect a problem now, you can copy the old database back over the new one
and edit your `nvr` script to use the corresponding older Docker image. If
you detect a problem after enabling read-write operation, a restore will be
more complicated.

Once you're satisfied, restart the system in read-write mode:

```
$ nvr stop
$ nvr rm
$ nvr run
```

Hopefully your system is functioning correctly. If not, there are two options
for restore; neither are easy:

   * go back to your old database. There will be two classes of problems:
        * If the new system deleted any recordings, the old system will
          incorrectly believe they are still present. You could wait until all
          existing files are rotated away, or you could try to delete them
          manually from the database.
        * if the new system created any recordings, the old system will not
          know about them and will not delete them. Your disk may become full.
          You should find some way to discover these files and manually delete
          them.
   * undo the changes by hand. There's no documentation on this; you'll need
     to read the code and come up with a reverse transformation.

The `nvr check` command will show you what problems exist on your system.

### Unversioned to version 0

Early versions of Moonfire NVR (prior to 2016-12-20) did not include the
version information in the schema. You can manually add this information to
your schema using the `sqlite3` commandline. This process is backward
compatible, meaning that software versions that accept an unversioned database
will also accept a version 0 database.

Version 0 makes two changes:

   * it adds schema versioning, as described above.
   * it adds a column (`video_sync_samples`) to a database index to speed up
     certain operations.

There's a special procedure for this upgrade. The good news is that a backup
is unnecessary; there's no risk with this procedure.

First ensure Moonfire NVR is not running as described in the general procedure
above.

Then use `sqlite3` to manually edit the database. The default
path is `/var/lib/moonfire-nvr/db/db`; if you've specified a different
`--db_dir`, use that directory with a suffix of `/db`.

    $ sudo -u moonfire-nvr sqlite3 /var/lib/moonfire-nvr/db/db
    sqlite3>

At the prompt, run the following commands:

```sql
begin transaction;

create table version (
  id integer primary key,
  unix_time integer not null,
  notes text
);

insert into version values (0, cast(strftime('%s', 'now') as int),
                            'manual upgrade to version 0');

drop index recording_cover;

create index recording_cover on recording (
  camera_id,
  start_time_90k,
  duration_90k,
  video_samples,
  video_sample_entry_id,
  sample_file_bytes
);

commit transaction;
```

When you are done, you can restart the service via `systemctl` and continue
using it with your existing or new version of Moonfire NVR.

### Version 0 to version 1

Version 1 makes several changes to the recording tables and indices. These
changes allow overlapping recordings to be unambiguously listed and viewed.
They also reduce the amount of I/O; in one test of retrieving playback
indexes, the number of (mostly 1024-byte) read syscalls on the database
dropped from 605 to 39.

The general upgrade procedure applies to this upgrade.

### Version 1 to version 2 to version 3

This upgrade affects the sample file directory as well as the database. Thus,
the restore procedure written above of simply copying back the database is
insufficient. To do a full restore, you would need to back up and restore the
sample file directory as well. This directory is considerably larger, so
consider an alternate procedure of crossing your fingers, and being prepared
to start over from scratch if there's a problem.

Version 2 represents a half-finished upgrade from version 1 to version 3; it
is never used.

Version 3 adds over version 1:

*   user authentication
*   recording of sub streams (splits a new `stream` table out of `camera`)
*   a per-stream knob `flush_if_sec` meant to reduce database commits (and
    thus SSD write cycles). This improves practicality of many streams.
*   support for multiple sample file directories, to take advantage of
    multiple hard drives (or multiple RAID volumes).
*   an interlock between database and sample file directories to avoid various
    mixups that could cause data integrity problems.
*   recording the RFC-6381 codec associated with a video sample entry, so that
    logic for determining this is no longer needed as part of the database
    layer.
*   a simpler sample file directory layout in which files are represented by
    the same sequentially increasing id as in the database, rather than a
    separate uuid which has to be reserved in advance.
*   additional timestamp fields which may be useful in diagnosing/correcting
    time jumps/inconsistencies.

### Version 3 to version 4 to version 5

This upgrade affects the SQLite database and the sample file directory's
`meta` files.

Version 4 represents a half-finished upgrade from version 3 to version 5.

Version 5 adds over version 3:

*   permissions for users and sessions. Existing users will have only the
    `view_video` permission, matching their previous behavior.
*   the `signals` schema, used to store status of signals such as camera
    motion detection, security system zones, etc. Note that while the schema
    is stable for now, there's no support yet for configuring signals via
    the `moonfire-nvr config` subcommand.
*   the ability to recover from a completely full sample file directory (#65)
    without manual intervention.

### Version 6

This upgrade affects only the SQLite database.

Version 6 adds over version 5:

*   metadata about the pixel aspect ratio to properly support
    [anamorphic](https://en.wikipedia.org/wiki/Anamorphic_widescreen) "sub"
    streams.
*   hashes in Blake3 rather than older SHA-1 (for file integrity checksums)
    or Blake2b (for sessions).
*   for each recording row, the cumulative total duration and "runs" recorded
    before it on that stream. This is useful for MediaSourceExtension-based
    web browser UIs when setting timestamps of video segments in the
    SourceBuffer.
*   decoupled "wall time" and "media time" of recoridngs, as a step toward
    implementing audio support without giving up clock frequency adjustment. See
    [this comment](https://github.com/scottlamb/moonfire-nvr/issues/34#issuecomment-651548468).

On upgrading to this version, sessions will be revoked.
