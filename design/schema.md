# Moonfire NVR Storage Schema

Status: **current**.

This is the initial design for the most fundamental parts of the Moonfire NVR
storage schema. See also [guide/schema.md](../guide/schema.md) for more
administrator-focused documentation.

## Objective

Goals:

* record streams from modern ONVIF/PSIA IP security cameras
* support several cameras
* maintain full fidelity of incoming compressed video streams
* record continuously
* support on-demand serving in different file formats / protocols
  (such as standard .mp4 files for arbitrary timespans, fragmented .mp4 files
  for MPEG-DASH or HTML5 Video Source Extensions, MPEG-TS files for HTTP Live
  Streaming, and "trick play" RTSP)
* annotate camera timelines with metadata
  (such as motion detection, security alarm events, etc)
* retain video segments with ~1-minute granularity based on metadata
  (e.g., extend retention of motion events)
* take advantage of compact, inexpensive, low-power, commonly-available
  hardware such as the $35 [Raspberry Pi 2 Model B][pi2]
* support high- and low-bandwidth playback
* support near-live playback (~second old), including "trick play"
* allow verifying database consistency with an `fsck` tool

Non-goals:

* record streams from older cameras: JPEG/MJPEG USB "webcams" and analog
  security cameras/capture cards
* allow users to directly access or manipulate the stored data with standard
  video or filesystem tools
* support H.264 features not used by common IP camera encoders, such as
  B-frames and Periodic Infra Refresh.
* support recovering the last ~minute of video after a crash or power loss

Possible future goals:

* record audio and/or other types of timestamped samples (such as
  [Xandem][xandem] tomography data).

### Cameras

Inexpensive modern ONVIF/PSIA IP security cameras, such as the $100
[Hikvision DS-2CD2032-I][hikcam], support two H.264-encoded RTSP
streams. They have many customizable settings, such as resolution, frame rate,
compression quality, maximum bitrate, I-frame interval. A typical setup might be
as follows:

* the high-quality "main" stream as 1080p/30fps, 3000 kbps.
  This stream is well-suited to local viewing or forensics.
* the low-bandwidth "sub" stream as 704x480/10fps, 100 kbps.
  This stream may be preferred for mobile/remote viewing, when viewing several
  streams side-by-side, and for real-time computer vision (such as salient
  motion detection).

The dual pre-encoded H.264 video streams provide a tremendous advantage over
older camera models (which provided raw video or JPEG-encoded frames) because
the encoding is prohibitively expensive in multi-camera setups.
[libx264][libx264] supports "encoding 4 or more 1080p streams in realtime on a
single consumer-level computer", but this does not apply to the low-cost devices
Moonfire NVR targets. In fact, even decoding can be expensive on the
full-quality streams, enough to challenge the feasibility of on-NVR motion
detection. It's valuable to have the "sub" stream for this purpose.

The table below shows cost of processing a single stream, as a percentage of the
whole processor ((user+sys) time / video duration / CPU cores). **TODO:** try
different quality settings as well.

Decode:

    $ time ffmpeg -y -threads 1 -i input.mp4 \
                  -f null /dev/null

Combo (Decode + encode with libx264):

    $ time ffmpeg -y -threads 1 -i input.mp4 \
                  -c:v libx264 -preset ultrafast -threads 1 -f mp4 /dev/null


| Processor                     | 1080p30 decode | 1080p30 combo | 704x480p10 decode | 704x480p10 combo |
| :---------------------------- | -------------: | ------------: | ----------------: | ---------------: |
| [Intel i7-2635QM][2635QM]     |           6.0% |         23.7% |              0.2% |             1.0% |
| [Intel Atom C2538][C2538]     |          16.7% |         58.1% |              0.7% |             3.0% |
| [Raspberry Pi 2 Model B][pi2] |          68.4% |    **230.1%** |              2.9% |            11.7% |

Hardware-accelerated decoding/encoding is possible in some cases (VAAPI on the
Intel processors, or OpenMAX on the Raspberry Pi), but similarly it would not be
possible to have several high-quality streams without using the camera's
encoding. **TODO:** get numbers.

### Hard drives ###

With current hard drives prices (see [WD Purple][wdpurple] prices below), it's
cost-effective to store a month or more of high-quality video, at roughly 1
camera-month per TB.

| Capacity | Price |
| -------: | ----: |
|     1 TB |   $61 |
|     2 TB |   $82 |
|     3 TB |  $107 |
|     4 TB |  $157 |
|     6 TB |  $240 |

Typical sequential bandwidth is >100 MB/sec, more than that required by over a
hundred streams at 3 Mbps. The concern is seek times: a [WD20EURS][wd20eurs]
appears to require 20 ms per sequential random access (across the full range
of the disk), as measured with [seeker][seeker]. Put another way, the drive is
only capable of 50 random accesses per second, and each one takes time that
otherwise could be used to transfer 2+ MB. The constrained resource, *disk
time fraction*, can be bounded as follows:

    disk time fraction <= (seek rate) / (50 seeks/sec) +
                          (bandwidth) / (100 MB/sec)

## Overview

Moonfire NVR divides video streams into 1-minute recordings. These boundaries
are invisible to the user. On playback, the UI moves from one recording to
another seamlessly. When exporting video, recordings are automatically spliced
together.

Each recording is stored in two places:

* a sample file directory, intended to be stored on spinning disk.
  Each file in this directory is simply a concatenation of the compressed,
  timestamped video samples (also called "packets" or encoded frames), as
  received from the camera. In MPEG-4 terminology (see [ISO
  14496-12][iso-14496-12]), this is the contents of a `mdat` box for a `.mp4`
  file representing the segment. These files do not contain framing data (start
  and end byte offsets of samples) and thus are not meant to be decoded on
  their own.
* the `recording` table in a [SQLite3][sqlite3] database, intended to be
  stored on flash if possible. A row in this table contains all the metadata
  associated with the segment, including the sample-by-sample contents of the
  MPEG-4 `stbl` box. At 30 fps, a row is expected to require roughly 4 KB of
  storage (2 bytes per sample, plus some fixed overhead).

Putting the metadata on flash means metadata operations can be fast
(sub-millisecond random access, with parallelism) and do not take precious
disk time fraction away from accessing sample data. Disk time can be saved for
long sequential accesses. Assuming filesystem metadata is cached, Moonfire NVR
can seek directly to the correct sample.

To avoid a burst of seeks every minute, rotation times will be staggered. For
example, if there are two cameras (A and B), camera A's main stream might
switch to a new recording at :00 seconds past the minute, B's main stream at
:15 seconds past the minute, and likewise the sub streams, as shown below.

| camera | stream | switchover |
| :----- | :----- | ---------: |
| A      | main   |   xx:xx:00 |
| B      | main   |   xx:xx:15 |
| A      | sub    |   xx:xx:30 |
| B      | sub    |   xx:xx:45 |

## Detailed design

### SQLite3

All metadata, including the `recording` table and others, will be stored in
the SQLite3 database using [write-ahead logging][sqlite3-wal]. There are
several reasons for this decision:

* No user administration required. SQLite3, unlike its heavier-weight friends
  MySQL and PostgreSQL, can be completely internal to the application. In many
  applications, end users are unaware of the existence of a RDBMS, and
  Moonfire NVR should be no exception.
* Correctness. It's relatively easy to make guarantees about the state of an
  ACID database, and SQLite3 in particular has a robust implementation. (See
  [Files Are Hard][file-consistency].)
* Developer ease and familiarity. SQL-based RDBMSs are quite common and
  provide a lot of high-level constructs that ease development. SQLite3 in
  particular is ubiquitous. Contributors are likely to come with some
  understanding of the database, and there are many resources to learn more.

Total database size is expected to be roughly 4 KB per minute at 30 fps, or
1 GB for six camera-months of video. This will easily fit on a modest flash
device. Given the fast storage and modest size, the database is not expected
to be a performance bottleneck.

### Duration of recordings

There are many constraints that influenced the choice of 1 minute as the
duration of recordings.

* Per-recording metadata size. There is a fixed component to the size of each
  row, including the starting/ending timestamps, sample file UUID, etc. This
  should not cause the database to be too large to fit on low-cost flash
  devices. As described in the previous section, with 1 minute recordings the
  size is quite modest.
* Disk seeks. Sample files should be large enough that even during
  simultaneous recording and playback of several streams, the disk seeks
  incurred when switching from one file to another should not be significant.
  At the extreme, a sample file per frame could cause an unacceptable 240
  seeks per second just to record 8 30 fps streams. At one minute recording
  time, 16 recording streams (2 per each of 8 cameras) and 4 playback streams
  would cause on average 20 seeks per minute, or under 1% disk time.
* Internal fragmentation. Common Linux filesystems have a block size of 4 KiB
  (see `statvfs.f_frsize`). Up to this much space per file will be wasted at
  the end of each file. At the bitrates described in "Background", this is an
  insignicant .02% waste for main streams and .5% waste for sub streams.
* Number of "slices" in .mp4 files. As described [below](#on-demand),
  `.mp4` files will be constructed on-demand for export. It should be
  possible to export an hours-long segment without too much overhead. In
  particular, it must be possible to iterate through all the recordings,
  assemble the list of slices, and calculate offsets and total size. One
  minute seems acceptable; though we will watch this as work proceeds.
* Crashes. On program crash or power loss, ideally it's acceptable to simply
  discard any recordings in progress rather than add a checkpointing scheme.
* Granularity of retention. It should be possible to extend retention time
  around motion events without forcing retention of too much additional data
  or copying bytes around on disk.

The design avoids the need for the following constraints:

* Dealing with events crossing segment boundaries. This is meant to be
  invisible.
* Serving close to live. It's possible to serve a recording as it is being
  written.

### Lifecycle of a sample file directory

One major disadvantage to splitting the state in two (the SQLite3 database in
flash and the sample file directories on spinning disk) is the possibility of
inconsistency. There are many ways this could arise:

* a sample file directory's disk is unexpectedly not mounted due to hardware
  failure or misconfiguration.
* the administrator mixing up the mount points of two filesystems holding
  different sample file directories.
* the administrator renaming a sample file directory without updating the database.
* the administrator restoring the database from backup but not the sample file
  directory, or vice versa.
* the administrator providing two sample file directory paths pointed at the
  same inode via symlinks or non-canonical paths. (Note that flock(2) has a
  design flaw in which multiple file descriptors can share a lock, so the current
  locking scheme is not sufficient to detect this otherwise.)
* database and sample file directories forked from the same version, opened
  the same number of times, then crossed.

To combat this, each sample file directory has some metadata its database row
and stored file called `meta`. These track uuids associated with the database
and directory to avoid mixups. They also track sequence numbers and uuids
associated with "opens": each time the database has been opened in read/write
mode.

```sql
create table open (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16)
);

create table sample_file_dir (
  id integer primary key,
  path text unique not null,
  uuid blob unique not null check (length(uuid) = 16),

  -- The last (read/write) open of this directory which fully completed.
  -- See schema.proto:DirMeta for a more complete description.
  last_complete_open_id integer references open (id)
);
```

```proto
message DirMeta {
  // A uuid associated with the database, in binary form. dir_uuid is strictly
  // more powerful, but it improves diagnostics to know if the directory
  // belongs to the expected database at all or not.
  bytes db_uuid = 1;

  // A uuid associated with the directory itself.
  bytes dir_uuid = 2;

  // Corresponds to an entry in the `open` database table.
  message Open {
    uint32 id = 1;
    bytes uuid = 2;
  }

  // The last open that was known to be recorded in the database as completed.
  // Absent if this has never happened. Note this can backtrack in exactly one
  // scenario: when deleting the directory, after all associated files have
  // been deleted, last_complete_open can be moved to in_progress_open.
  Open last_complete_open = 3;

  // The last run which is in progress, if different from last_complete_open.
  // This may or may not have been recorded in the database, but it's
  // guaranteed that no data has yet been written by this open.
  Open in_progress_open = 4;
}
```

These are updated through procedures below:

*Write the metadata file*

This is a sub-procedure used in several places below.

Precondition: the directory's lock is held with `LOCK_EX` (exclusive).

  1. Write a new `meta.tmp` (opened with `O_CREAT|O_TRUNC` to discard an
     existing temporary file if any).
  2. `fsync` the `meta.tmp` file descriptor.
  3. `rename` `meta.tmp` to `meta`.
  4. `fsync` the directory.

*Open the database as read-only*

  1. Lock the database directory with `LOCK_SH` (shared).
  2. Open the SQLite database with `SQLITE_OPEN_READ_ONLY`.

*Open the database as read-write*

  1. Lock the database directory with `LOCK_EX` (exclusive).
  2. Open the SQLite database with `SQLITE_OPEN_READ_WRITE`.
  3. Insert a new `open` table row with the new sequence number and uuid.

*Create a sample file directory*

Precondition: database open read-write.

  1. Lock the sample file directory with `LOCK_EX` (exclusive).
  2. Verify there is no metadata file or `last_complete_open` is unset.
  3. Write new metadata file with a fresh `dir_uuid` and a `in_progress_open`
     matching the database's current open.
  4. Add a matching row to the database with `last_complete_open_id` matching
     the current open.
  5. Update the metadata file to move `in_progress_open` to
     `last_complete_open`.

*Open a sample file directory read-only*

Precondition: database open (read-only or read-write).

  1. Lock the sample file directory with `LOCK_SH` (shared).
  2. Verify the metadata file matches the database:
        * database uuid matches.
        * dir uuid matches.
        * if the database's `last_complete_open` is set, it must match the
          directory's `last_complete_open` or `in_progress_open`.
        * if the database's `last_complete_open` is absent, the directory's
          must be as well.

*Open a sample file directory read-write*

Precondition: database open read-write.

  1. Lock the sample file directory with `LOCK_EX` (exclusive).
  2. Verify the metadata file matches the database (as above).
  3. Update the metadata file with `in_progress_open` matching the current
     open.
  3. Update the database row with `last_complete_open_id` matching the current
     open.
  4. Update the metadata file with `last_complete_open` rather than
     `in_progress_open`.
  5. Run the recording startup procedure for this directory.

*Close a sample file directory*

  1. Drop the sample file directory lock.

*Delete a sample file directory*

  1. Remove all sample files (of all three categories described below:
     `recording` table rows, `garbage` table rows, and files with recording
     ids >= their stream's `next_recording_id`); see "delete a recording"
     procedure below.
  2. Rewrite the directory metadata with `in_progress_open` set to the current open,
     `last_complete_open` cleared.
  3. Delete the directory's row from the database.

### Lifecycle of a recording

Because a major part of the recording state is outside the SQL database, care
must be taken to guarantee consistency and durability. Moonfire NVR maintains
three invariants about sample files:

  1. `recording` table rows have sample files on disk with the indicated size
     and SHA-1 hash.
  2. Exactly one of the following statements is true for every sample file:
        * It has a `recording` table row.
        * It has a `garbage` table row.
        * Its recording id is greater than or equal to the `next_recording_id`
          for its stream.
  3. After an orderly shutdown of Moonfire NVR, there is a `recording` table row
     for every sample file, even if there have been previous crashes.

The first invariant provides certainty that a recording is properly stored. It
would be prohibitively expensive to verify hashes on demand (when listing or
serving recordings), or in some cases even to verify the size of the files via
`stat()` calls.

The second invariant improves auditability of the database and sample file
directory.

The third invariant prevents accumulation of garbage files which could fill the
drive and stop recording.

These invariants are updated through the following procedure:

*Create a recording:*

1. Write the sample file, aborting if `open(..., O\_WRONLY|O\_CREATE|O\_EXCL)`
   fails with `EEXIST`.
3. `fsync()` the sample file.
4. `fsync()` the sample file directory.
5. Insert the `recording` row, marking its size and SHA-1 hash in the process.

*Delete a recording:*

1. Replace the `recording` row with a `garbage` row.
2. `unlink()` the sample file, warning on `ENOENT`. (This would indicate
   invariant #2 is false.)
3. `fsync()` the sample file directory.
4. Delete the `garbage` row.

*Startup (crash recovery):*

1. Acquire a lock to guarantee this is the only Moonfire NVR process running
   against the given database. This lock is not released until program shutdown.
2. Query `garbage` table and `next_recording_id` field in the `stream` table.
3. `unlink()` all the sample files associated with garbage rows, ignoring
   `ENOENT.
4. For each stream, `unlink()` all the existing files with recording ids >=
   `next_recording_id`.
4. `fsync()` the sample file directory.
5. Delete all rows from the `garbage` table.

The procedures can be batched: while for a given recording, the steps must be
strictly ordered, multiple recordings can be proceeding through the steps
simultaneously. In particular, there is no need to hurry syncing deletions to
disk, so deletion steps #3 and #4 can be done opportunistically if it's
desirable to avoid extra disk seeks or flash write cycles.

It'd also be possible to conserve some partial recordings. Moonfire NVR could,
as a recording is written, record the latest sample tables,
size, and hash fields without marking the recording as fully written. On
startup, the file would be truncated to match and then the recording marked
as fully written. The file would either have to be synced prior to each update
(to guarantee it is at least as new as the row) or multiple checkpoints would
be kept, using the last one with a correct hash (if any) on a best-effort
basis. However, this may not be worth the complexity; it's simpler to just
keep recording time short enough that losing partial recordings is not a
problem.

### Verifying invariants

There should be a means to verify the invariants above. There are three
possible levels of verification:

1. Compare presence of sample files.
2. Compare size of sample files.
3. Compare hashes of sample files.

Consider a database with a 6 camera-months of recordings at 3.1 Mbps (for
both main and sub streams). There would be 0.5 million files, taking 5.9 TB.
The times are roughly:

| level    | operation   |     time |
| :------- | :---------- | -------: |
| presence | `readdir()` |  ~19 sec |
| size     | `fstat()`   | ~120 sec |
| hash     | `read()`    | ~8 hours |

The `readdir()` and `fstat()` times can be tested simply:

    $ mkdir testdir
    $ cd testdir
    $ seq 1 $[60*24*365*6/12*2] | xargs touch
    $ sudo sh -c 'echo 1 > /proc/sys/vm/drop_caches'
    $ time ls -1 -f | wc -l
    $ sudo sh -c 'echo 1 > /proc/sys/vm/drop_caches'
    $ time ls -1 -f --size | wc -l

    (The system calls used by `ls` can be verified through strace.)

The hash verification time is easiest to calculate: reading 5.9 TB at 100
MB/sec takes about 8 hours. On some systems, it will be even slower. On
the Raspberry Pi 2, flash, network, and disk are all on the same USB 2.0 bus
(see [Raspberry Pi 2 NAS Experiment HOWTO][pi-2-nas]). Disk throughput seems
to be about 25 MB/sec on an idle system (~40% of the theoretical 480
Mbit/sec). Therefore the process will take over a day.

The presence check is fast enough that it seems reasonable to simply always
perform it on startup. Size could be checked with a verification command used
for more extensive verification, such as before and after schema upgrades.
Hash checks could be performed in a rare offline data recovery mechanism or in
the background at low priority.

### Recording table

The snippet below is a illustrative excerpt of the SQLite schema; see
`schema.sql` for the authoritative, up-to-date version.

    -- A single, typically 60-second, recorded segment of video.
    create table recording (
      id integer primary key,
      open_id integer references open (id),
      camera_id integer references camera (id) not null,

      sample_file_uuid blob unique not null,
      sample_file_sha1 blob,
      sample_file_size integer,

      -- The starting time and duration of the recording, in 90 kHz units since
      -- 1970-01-01 00:00:00 UTC.
      start_time_90k integer not null,
      duration_90k integer,

      video_samples integer,
      video_sample_entry_id blob references visual_sample_entry (id),
      video_index blob,

      ...
    );

    -- A concrete box derived from a ISO/IEC 14496-12 section 8.5.2
    -- VisualSampleEntry box. Describes the codec, width, height, etc.
    create table visual_sample_entry (
      -- A SHA-1 hash of |bytes|.
      sha1 blob primary key,

      -- The width and height in pixels; must match values within
      |sample_entry_bytes|.
      width integer,
      height integer,

      -- A serialized SampleEntry box, including the leading length and box
      -- type (avcC in the case of H.264).
      data blob
    );

As mentioned by the `start_time_90k` field above, recordings use a 90 kHz time
base. This matches the RTP timestamp frequency used for H.264 and other video
encodings. See [RFC 3551][rfc-3551] section 5 for an explanation of this
choice.

It's tempting to downscale to a coarser timebase, rounding as necessary, in
the name of a more compact encoding of `video_index`. (By having timestamp
deltas near zero and borrowing some of the timestamp varint to represent
additional bits of the size deltas, it's possible to use barely more than 2
bytes per frame on a typical recording. **TODO:** recalculate database size
estimates above, which were made using this technique.) But matching the input
timebase is the most understandable approach and leaves the most flexibility
available for handling timestamps encoded in RTCP Sender Report messages. In
practice, a database size of two gigabytes rather than one is unlikely to cause
problems.

One likely point of difficulty is reliably mapping recordings to wall clock
time. (This may be the subject of a separate design doc later.) In an ideal
world, the NVR and cameras would each be closely synced to a reliable NTP time
reference, time would advance at a consistent rate, time would never jump
forward or backward, each transmission would take bounded time, and cameras
would reliably send RTCP Sender Reports. In reality, none of that is likely to
be consistently true. For example, Hikvision cameras send RTCP Sender Reports
only with certain firmware versions (see [thread][hikvision-sr]). Most likely
it will be useful to have any available clock/timing information for
diagnosing problems, such as the following:

* the NVR's wall clock time
* the NVR's NTP server sync status
* the NVR's uptime
* the camera's time as of the RTP play response
* the camera's time as of any RTCP Sender Reports, and the corresponding RTP
  timestamps

#### `video_index`

The `video_index` field conceptually holds three pieces of information about
the samples:

1. the duration (in 90kHz units) of each sample
2. the byte size of each sample
3. which samples are "sync samples" (aka key frames or I-frames)

These correspond to [ISO/IEC 14496-12][iso-14496-12] `stts` (TimeToSampleBox,
section 8.6.1.2), `stsz` (SampleSizeBox, section 8.7.3), and `stss`
(SyncSampleBox, section 8.6.2) boxes, respectively.

Currently the `stsc` (SampleToChunkBox, section 8.7.4) information is implied:
all samples are in a single chunk from the beginning of the file to the end.
If in the future support for interleaved audio is added, there will be a new
blob field with chunk information. **TODO:** can audio data really be sliced
to fit the visual samples like this?

The index is structured as two [varints][varints] per sample. The first varint
represents the delta between this frame's duration and the previous frame's,
in [zigzag][zigzag] form. The low bit is borrowed to indicate if this frame
is a key frame. The second varint represents the delta between this frame's
duration and the duration of the last frame of the same type (key or non-key).
This encoding is chosen so that values will be near zero, and thus the varints
will be at their most compact possible form. An index might be written by the
following pseudocode:

    prev_duration = 0
    prev_bytes_key = 0
    prev_bytes_nonkey = 0
    for each frame:
      duration_delta = duration - prev_duration
      bytes_delta = bytes - (is_key ? prev_bytes_key : prev_bytes_nonkey)
      prev_duration_ms = duration_ms
      if key: prev_bytes_key = bytes else: prev_bytes_nonkey = bytes
      PutVarint((Zigzag(duration_delta) << 1) | is_key)
      PutVarint(Zigzag(bytes_delta)

See also the example below:

|                 |    frame 1 | frame 2 | frame 3 | frame 4 | frame 5 |
| :-------------- | ---------: | ------: | ------: | ------: | ------: |
| duration        |         10 |       9 |      11 |      10 |      10 |
| is\_key         |          1 |       0 |       0 |       0 |       1 |
| bytes           |       1000 |      10 |      15 |      12 |    1050 |
| duration\_delta |         10 |      -1 |       2 |      -1 |       0 |
| bytes\_delta    |       1000 |      10 |       5 |      -3 |      50 |
| varint1         |         41 |       2 |       8 |       3 |       1 |
| varint2         |       2000 |      20 |      10 |       5 |     100 |
| encoded         | `29 d0 0f` | `02 14` | `08 0a` | `02 05` | `01 64` |

### <a href="on-demand"></a>On-demand `.mp4` construction

A major goal of this format is to support on-demand serving in various formats,
including two types of `.mp4` files:

* unfragmented `.mp4` files, for traditional video players.
* fragmented `.mp4` files for MPEG-DASH or HTML5 Media Source Extensions
  (see [Media Source ISO BMFF Byte Stream Format][media-bmff]), for
  a browser-based user interface.

This does not require writing new `.mp4` files to disk. In fact, HTTP range
requests (for "pseudo-streaming") can be satisfied on `.mp4` files aggregated
from several segments. The implementation details are outside the scope of this
document, but this is possible in part due to the use of an on-flash database
to store metadata and the simple, consistent format of sample indexes.

### Copyright

This file is part of Moonfire NVR, a security camera network video recorder.
Copyright (C) 2016 Scott Lamb <slamb@slamb.org>

This program is free software: you can redistribute it and/or modify
it under the terms of the GNU General Public License as published by
the Free Software Foundation, either version 3 of the License, or
(at your option) any later version.

In addition, as a special exception, the copyright holders give
permission to link the code of portions of this program with the
OpenSSL library under certain conditions as described in each
individual source file, and distribute linked combinations including
the two.

You must obey the GNU General Public License in all respects for all
of the code used other than OpenSSL. If you modify file(s) with this
exception, you may extend this exception to your version of the
file(s), but you are not obligated to do so. If you do not wish to do
so, delete this exception statement from your version. If you delete
this exception statement from all source files in the program, then
also delete it here.

This program is distributed in the hope that it will be useful,
but WITHOUT ANY WARRANTY; without even the implied warranty of
MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
GNU General Public License for more details.

You should have received a copy of the GNU General Public License
along with this program.  If not, see <http://www.gnu.org/licenses/>.

[pi2]: https://www.raspberrypi.org/products/raspberry-pi-2-model-b/
[xandem]: http://www.xandemhome.com/
[hikcam]: http://overseas.hikvision.com/us/Products_accessries_10533_i7696.html
[libx264]: http://www.videolan.org/developers/x264.html
[2635QM]: http://ark.intel.com/products/53463/Intel-Core-i7-2635QM-Processor-6M-Cache-up-to-2_90-GHz
[C2538]: http://ark.intel.com/products/77981/Intel-Atom-Processor-C2538-2M-Cache-2_40-GHz
[wdpurple]: http://www.wdc.com/en/products/products.aspx?id=1210
[wd20eurs]: http://www.wdc.com/wdproducts/library/SpecSheet/ENG/2879-701250.pdf
[seeker]: http://www.linuxinsight.com/how_fast_is_your_disk.html
[rfc-3551]: https://tools.ietf.org/html/rfc3551
[hikvision-sr]: http://www.cctvforum.com/viewtopic.php?f=19&t=44534
[iso-14496-12]: http://www.iso.org/iso/home/store/catalogue_ics/catalogue_detail_ics.htm?csnumber=68960
[sqlite3]: https://www.sqlite.org/
[sqlite3-wal]: https://www.sqlite.org/wal.html
[file-consistency]: http://danluu.com/file-consistency/
[pi-2-nas]: http://www.mikronauts.com/raspberry-pi/raspberry-pi-2-nas-experiment-howto/
[varints]: https://developers.google.com/protocol-buffers/docs/encoding#varints
[zigzag]: https://developers.google.com/protocol-buffers/docs/encoding#types
[media-bmff]: https://w3c.github.io/media-source/isobmff-byte-stream-format.html
