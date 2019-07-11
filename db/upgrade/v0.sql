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

--pragma journal_mode = wal;

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
  uuid blob unique,-- not null check (length(uuid) = 16),

  -- A short name of the camera, used in log messages.
  short_name text,-- not null,

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
  retain_bytes integer not null check (retain_bytes >= 0)
);

-- Each row represents a single completed recorded segment of video.
-- Recordings are typically ~60 seconds; never more than 5 minutes.
create table recording (
  id integer primary key,
  camera_id integer references camera (id) not null,

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
  -- the recording. Large values would indicate that the local time has jumped
  -- during recording or that the local time and camera time frequencies do
  -- not match.
  local_time_delta_90k integer not null,

  video_samples integer not null check (video_samples > 0),
  video_sync_samples integer not null check (video_samples > 0),
  video_sample_entry_id integer references video_sample_entry (id),

  sample_file_uuid blob not null check (length(sample_file_uuid) = 16),
  sample_file_sha1 blob not null check (length(sample_file_sha1) = 20),
  video_index blob not null check (length(video_index) > 0)
);

create index recording_cover on recording (
  -- Typical queries use "where camera_id = ? order by start_time_90k (desc)?".
  camera_id,
  start_time_90k,

  -- These fields are not used for ordering; they cover most queries so
  -- that only database verification and actual viewing of recordings need
  -- to consult the underlying row.
  duration_90k,
  video_samples,
  video_sync_samples,
  video_sample_entry_id,
  sample_file_bytes
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
             values (0,  cast(strftime('%s', 'now') as int), 'db creation');
