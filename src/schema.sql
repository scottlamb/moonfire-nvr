-- This file is part of Moonfire NVR, a security camera digital video recorder.
-- Copyright (C) 2016 Scott Lamb <slamb@slamb.org>
--
-- This program is free software: you can redistribute it and/or modify
-- it under the terms of the GNU General Public License as published by
-- the Free Software Foundation, either version 3 of the License, or
-- (at your option) any later version.
--
-- In addition, as a special exception, the copyright holders give
-- permission to link the code of portions of this program with the
-- OpenSSL library under certain conditions as described in each
-- individual source file, and distribute linked combinations including
-- the two.
--
-- You must obey the GNU General Public License in all respects for all
-- of the code used other than OpenSSL. If you modify file(s) with this
-- exception, you may extend this exception to your version of the
-- file(s), but you are not obligated to do so. If you do not wish to do
-- so, delete this exception statement from your version. If you delete
-- this exception statement from all source files in the program, then
-- also delete it here.
--
-- This program is distributed in the hope that it will be useful,
-- but WITHOUT ANY WARRANTY; without even the implied warranty of
-- MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
-- GNU General Public License for more details.
--
-- You should have received a copy of the GNU General Public License
-- along with this program.  If not, see <http://www.gnu.org/licenses/>.
--
-- schema.sql: SQLite3 database schema for Moonfire NVR.
-- See also design/schema.md.

-- This table tracks the schema version.
-- There is one row for the initial database creation (inserted below, after the
-- create statements) and one for each upgrade procedure (if any).
create table version (
  id integer primary key,

  -- The unix time as of the creation/upgrade, as determined by
  -- cast(strftime('%s', 'now') as int).
  unix_time integer not null,

  -- Optional notes on the creation/upgrade; could include the binary version.
  notes text
);

create table camera (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16),

  -- A short name of the camera, used in log messages.
  short_name text not null,

  -- A short description of the camera.
  description text,

  -- The host (or IP address) to use in rtsp:// URLs when accessing the camera.
  host text,

  -- The username to use when accessing the camera.
  -- If empty, no username or password will be supplied.
  username text,

  -- The password to use when accessing the camera.
  password text,

  -- The path (starting with "/") to use in rtsp:// URLs to reference this
  -- camera's "main" (full-quality) video stream.
  main_rtsp_path text,

  -- The path (starting with "/") to use in rtsp:// URLs to reference this
  -- camera's "sub" (low-bandwidth) video stream.
  sub_rtsp_path text,

  -- The number of bytes of video to retain, excluding the currently-recording
  -- file. Older files will be deleted as necessary to stay within this limit.
  retain_bytes integer not null check (retain_bytes >= 0),

  -- The low 32 bits of the next recording id to assign for this camera.
  -- Typically this is the maximum current recording + 1, but it does
  -- not decrease if that recording is deleted.
  next_recording_id integer not null check (next_recording_id >= 0)
);

-- Each row represents a single completed recorded segment of video.
-- Recordings are typically ~60 seconds; never more than 5 minutes.
create table recording (
  -- The high 32 bits of composite_id are taken from the camera's id, which
  -- improves locality. The low 32 bits are taken from the camera's
  -- next_recording_id (which should be post-incremented in the same
  -- transaction). It'd be simpler to use a "without rowid" table and separate
  -- fields to make up the primary key, but
  -- <https://www.sqlite.org/withoutrowid.html> points out that "without rowid"
  -- is not appropriate when the average row size is in excess of 50 bytes.
  -- recording_cover rows (which match this id format) are typically 1--5 KiB.
  composite_id integer primary key,

  -- This field is redundant with id above, but used to enforce the reference
  -- constraint and to structure the recording_start_time index.
  camera_id integer not null references camera (id),

  -- The offset of this recording within a run. 0 means this was the first
  -- recording made from a RTSP session. The start of the run has id
  -- (id-run_offset).
  run_offset integer not null,

  -- flags is a bitmask:
  --
  -- * 1, or "trailing zero", indicates that this recording is the last in a
  --   stream. As the duration of a sample is not known until the next sample
  --   is received, the final sample in this recording will have duration 0.
  flags integer not null,

  sample_file_bytes integer not null check (sample_file_bytes > 0),

  -- The starting time of the recording, in 90 kHz units since
  -- 1970-01-01 00:00:00 UTC. Currently on initial connection, this is taken
  -- from the local system time; on subsequent recordings, it exactly
  -- matches the previous recording's end time.
  start_time_90k integer not null check (start_time_90k > 0),

  -- The duration of the recording, in 90 kHz units.
  duration_90k integer not null
      check (duration_90k >= 0 and duration_90k < 5*60*90000),

  -- The number of 90 kHz units the local system time is ahead of the
  -- recording; negative numbers indicate the local system time is behind
  -- the recording. Large absolute values would indicate that the local time
  -- has jumped during recording or that the local time and camera time
  -- frequencies do not match.
  local_time_delta_90k integer not null,

  video_samples integer not null check (video_samples > 0),
  video_sync_samples integer not null check (video_sync_samples > 0),
  video_sample_entry_id integer references video_sample_entry (id),

  check (composite_id >> 32 = camera_id)
);

create index recording_cover on recording (
  -- Typical queries use "where camera_id = ? order by start_time_90k".
  camera_id,
  start_time_90k,

  -- These fields are not used for ordering; they cover most queries so
  -- that only database verification and actual viewing of recordings need
  -- to consult the underlying row.
  duration_90k,
  video_samples,
  video_sample_entry_id,
  sample_file_bytes
);

-- Large fields for a recording which are not needed when simply listing all
-- of the recordings in a given range. In particular, when serving a byte
-- range within a .mp4 file, the recording_playback row is needed for the
-- recording(s) corresponding to that particular byte range, needed, but the
-- recording rows suffice for all other recordings in the .mp4.
create table recording_playback (
  -- See description on recording table.
  composite_id integer primary key references recording (composite_id),

  -- The binary representation of the sample file's uuid. The canonical text
  -- representation of this uuid is the filename within the sample file dir.
  sample_file_uuid blob not null check (length(sample_file_uuid) = 16),

  -- The sha1 hash of the contents of the sample file.
  sample_file_sha1 blob not null check (length(sample_file_sha1) = 20),

  -- See design/schema.md#video_index for a description of this field.
  video_index blob not null check (length(video_index) > 0)
);

-- Files in the sample file directory which may be present but should simply be
-- discarded on startup. (Recordings which were never completed or have been
-- marked for completion.)
create table reserved_sample_files (
  uuid blob primary key check (length(uuid) = 16),
  state integer not null  -- 0 (writing) or 1 (deleted)
) without rowid;

-- A concrete box derived from a ISO/IEC 14496-12 section 8.5.2
-- VisualSampleEntry box. Describes the codec, width, height, etc.
create table video_sample_entry (
  id integer primary key,

  -- A SHA-1 hash of |bytes|.
  sha1 blob unique not null check (length(sha1) = 20),

  -- The width and height in pixels; must match values within
  -- |sample_entry_bytes|.
  width integer not null check (width > 0),
  height integer not null check (height > 0),

  -- The serialized box, including the leading length and box type (avcC in
  -- the case of H.264).
  data blob not null check (length(data) > 86)
);

insert into version (id, unix_time,                           notes)
             values (1,  cast(strftime('%s', 'now') as int), 'db creation');
