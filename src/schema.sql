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

create table camera (
  id integer primary key,
  uuid blob unique not null,

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
  retain_bytes integer
);

-- A single, typically 60-second, recorded segment of video.
create table recording (
  id integer primary key,
  camera_id integer references camera (id) not null,

  status integer not null,  -- 0 (WRITING), 1 (WRITTEN), or 2 (DELETING)

  sample_file_uuid blob unique not null,
  sample_file_sha1 blob,
  sample_file_size integer,

  -- The starting and ending time of the recording, in 90 kHz units since
  -- 1970-01-01 00:00:00 UTC.
  start_time_90k integer not null,
  end_time_90k integer,

  video_samples integer,
  video_sample_entry_sha1 blob references visual_sample_entry (sha1),
  video_index blob
);

-- A concrete box derived from a ISO/IEC 14496-12 section 8.5.2
-- VisualSampleEntry box. Describes the codec, width, height, etc.
create table visual_sample_entry (
  -- A SHA-1 hash of |bytes|.
  sha1 blob primary key,

  -- The width and height in pixels; must match values within
  -- |sample_entry_bytes|.
  width integer,
  height integer,

  -- A serialized SampleEntry box, including the leading length and box
  -- type (avcC in the case of H.264).
  bytes blob
);
