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

### Unversioned to version 0

Early versions of Moonfire NVR did not include the version information in the
schema. You can manually add this information to your schema using the
`sqlite3` commandline. This process is backward compatible, meaning that
software versions that accept an unversioned database will also accept a
version 0 database.

Version 0 makes two changes:

    * schema versioning, as described above.
    * adding a column (`video_sync_samples`) to a database index to speed up
      certain operations.

First ensure Moonfire NVR is not running; if you are using systemd with the
service name `moonfire-nvr`, you can do this as follows:

    $ sudo systemctl stop moonfire-nvr

The service takes a moment to shut down; wait until the following command
reports that it is not running:

    $ sudo systemctl status moonfire-nvr

Then use `sqlite3` to manually edit the database. The default path is
`/var/lib/moonfire-nvr/db/db`; if you've specified a different `--db_dir`,
use that directory with a suffix of `/db`.

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

When you are done, you can restart the service:

    $ sudo systemctl start moonfire-nvr
