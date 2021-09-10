-- This file is part of Moonfire NVR, a security camera network video recorder.
-- Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
-- SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.';

-- schema.sql: SQLite3 database schema for Moonfire NVR.
-- See also design/schema.md.

-- Database metadata. There should be exactly one row in this table.
create table meta (
  uuid blob not null check (length(uuid) = 16),

  -- The maximum number of entries in the signal_state table. If an update
  -- causes this to be exceeded, older times will be garbage collected to stay
  -- within the limit.
  max_signal_changes integer check (max_signal_changes >= 0)
);

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

-- Tracks every time the database has been opened in read/write mode.
-- This is used to ensure directories are in sync with the database (see
-- schema.proto:DirMeta), to disambiguate uncommitted recordings, and
-- potentially to understand time problems.
create table open (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16),

  -- Information about when / how long the database was open. These may be all
  -- null, for example in the open that represents all information written
  -- prior to database version 3.

  -- System time when the database was opened, in 90 kHz units since
  -- 1970-01-01 00:00:00Z excluding leap seconds.
  start_time_90k integer,

  -- System time when the database was closed or (on crash) last flushed.
  end_time_90k integer,

  -- How long the database was open. This is end_time_90k - start_time_90k if
  -- there were no time steps or leap seconds during this time.
  duration_90k integer
);

create table sample_file_dir (
  id integer primary key,
  path text unique not null,
  uuid blob unique not null check (length(uuid) = 16),

  -- The last (read/write) open of this directory which fully completed.
  -- See schema.proto:DirMeta for a more complete description.
  last_complete_open_id integer references open (id)
);

create table camera (
  id integer primary key,
  uuid blob unique not null check (length(uuid) = 16),

  -- A short name of the camera, used in log messages.
  short_name text not null,

  -- A serialized json.CameraConfig
  config text not null
);

create table stream (
  id integer primary key,
  camera_id integer not null references camera (id),
  sample_file_dir_id integer references sample_file_dir (id),
  type text not null check (type in ('main', 'sub', 'ext')),

  -- A serialized json.StreamConfig
  config text not null,

  -- The total number of recordings ever created on this stream, including
  -- deleted ones. This is used for assigning the next recording id.
  cum_recordings integer not null check (cum_recordings >= 0),

  -- The total media duration of all recordings ever created on this stream.
  cum_media_duration_90k integer not null check (cum_media_duration_90k >= 0),

  -- The total number of runs (recordings with run_offset = 0) ever created
  -- on this stream.
  cum_runs integer not null check (cum_runs >= 0),

  unique (camera_id, type)
);

-- Each row represents a single completed recorded segment of video.
-- Recordings are typically ~60 seconds; never more than 5 minutes.
create table recording (
  -- The high 32 bits of composite_id are taken from the stream's id, which
  -- improves locality. The low 32 bits are taken from the stream's
  -- cum_recordings (which should be post-incremented in the same
  -- transaction). It'd be simpler to use a "without rowid" table and separate
  -- fields to make up the primary key, but
  -- <https://www.sqlite.org/withoutrowid.html> points out that "without
  -- rowid" is not appropriate when the average row size is in excess of 50
  -- bytes. recording_cover rows (which match this id format) are typically
  -- 1--5 KiB.
  composite_id integer primary key,

  -- The open in which this was committed to the database. For a given
  -- composite_id, only one recording will ever be committed to the database,
  -- but in-memory state may reflect a recording which never gets committed.
  -- This field allows disambiguation in etags and such.
  open_id integer not null references open (id),

  -- This field is redundant with composite_id above, but used to enforce the
  -- reference constraint and to structure the recording_start_time index.
  stream_id integer not null references stream (id),

  -- The offset of this recording within a run. 0 means this was the first
  -- recording made from a RTSP session. The start of the run has composite_id
  -- (composite_id-run_offset).
  run_offset integer not null,

  -- flags is a bitmask:
  --
  -- * 1, or "trailing zero", indicates that this recording is the last in a
  --   stream. As the duration of a sample is not known until the next sample
  --   is received, the final sample in this recording will have duration 0.
  flags integer not null,

  sample_file_bytes integer not null check (sample_file_bytes > 0),

  -- The starting time of the recording, in 90 kHz units since
  -- 1970-01-01 00:00:00 UTC excluding leap seconds. Currently on initial
  -- connection, this is taken from the local system time; on subsequent
  -- recordings in a run, it exactly matches the previous recording's end
  -- time.
  start_time_90k integer not null check (start_time_90k > 0),

  -- The total duration of all previous recordings on this stream. This is
  -- returned in API requests and may be helpful for timestamps in a HTML
  -- MediaSourceExtensions SourceBuffer.
  prev_media_duration_90k integer not null
      check (prev_media_duration_90k >= 0),

  -- The total number of previous runs (rows in which run_offset = 0).
  prev_runs integer not null check (prev_runs >= 0),

  -- The wall-time duration of the recording, in 90 kHz units. This is the
  -- "corrected" duration.
  wall_duration_90k integer not null
      check (wall_duration_90k >= 0 and wall_duration_90k < 5*60*90000),

  -- The media-time duration of the recording, relative to wall_duration_90k.
  -- That is, media_duration_90k = wall_duration_90k + media_duration_delta_90k.
  media_duration_delta_90k integer not null,

  video_samples integer not null check (video_samples > 0),
  video_sync_samples integer not null check (video_sync_samples > 0),
  video_sample_entry_id integer references video_sample_entry (id),

  check (composite_id >> 32 = stream_id)
);

create index recording_cover on recording (
  -- Typical queries use "where stream_id = ? order by start_time_90k".
  stream_id,
  start_time_90k,

  -- These fields are not used for ordering; they cover most queries so
  -- that only database verification and actual viewing of recordings need
  -- to consult the underlying row.
  open_id,
  wall_duration_90k,
  media_duration_delta_90k,
  video_samples,
  video_sync_samples,
  video_sample_entry_id,
  sample_file_bytes,
  run_offset,
  flags
);

-- Fields which are only needed to check/correct database integrity problems
-- (such as incorrect timestamps).
create table recording_integrity (
  -- See description on recording table.
  composite_id integer primary key references recording (composite_id),

  -- The number of 90 kHz units the local system's monotonic clock has
  -- advanced more than the stated duration of recordings in a run since the
  -- first recording ended. Negative numbers indicate the local system time is
  -- behind the recording.
  --
  -- The first recording of a run (that is, one with run_offset=0) has null
  -- local_time_delta_90k because errors are assumed to
  -- be the result of initial buffering rather than frequency mismatch.
  --
  -- This value should be near 0 even on long runs in which the camera's clock
  -- and local system's clock frequency differ because each recording's delta
  -- is used to correct the durations of the next (up to 500 ppm error).
  local_time_delta_90k integer,

  -- The number of 90 kHz units the local system's monotonic clock had
  -- advanced since the database was opened, as of the start of recording.
  -- TODO: fill this in!
  local_time_since_open_90k integer,

  -- The difference between start_time_90k+duration_90k and a wall clock
  -- timestamp captured at end of this recording. This is meaningful for all
  -- recordings in a run, even the initial one (run_offset=0), because
  -- start_time_90k is derived from the wall time as of when recording
  -- starts, not when it ends.
  -- TODO: fill this in!
  wall_time_delta_90k integer,

  -- The (possibly truncated) raw blake3 hash of the contents of the sample
  -- file.
  sample_file_blake3 blob check (length(sample_file_blake3) <= 32)
);

-- Large fields for a recording which are needed ony for playback.
-- In particular, when serving a byte range within a .mp4 file, the
-- recording_playback row is needed for the recording(s) corresponding to that
-- particular byte range, needed, but the recording rows suffice for all other
-- recordings in the .mp4.
create table recording_playback (
  -- See description on recording table.
  composite_id integer primary key references recording (composite_id),

  -- See design/schema.md#video_index for a description of this field.
  video_index blob not null check (length(video_index) > 0)

  -- audio_index could be added here in the future.
);

-- Files which are to be deleted (may or may not still exist).
-- Note that besides these files, for each stream, any recordings >= its
-- cum_recordings should be discarded on startup.
create table garbage (
  -- This is _mostly_ redundant with composite_id, which contains the stream
  -- id and thus a linkage to the sample file directory. Listing it here
  -- explicitly means that streams can be deleted without losing the
  -- association of garbage to directory.
  sample_file_dir_id integer not null references sample_file_dir (id),

  -- See description on recording table.
  composite_id integer not null,

  -- Organize the table first by directory, as that's how it will be queried.
  primary key (sample_file_dir_id, composite_id)
) without rowid;

-- A concrete box derived from a ISO/IEC 14496-12 section 8.5.2
-- VisualSampleEntry box. Describes the codec, width, height, etc.
create table video_sample_entry (
  id integer primary key,

  -- The width and height in pixels; must match values within
  -- `sample_entry_bytes`.
  width integer not null check (width > 0),
  height integer not null check (height > 0),

  -- The codec in RFC-6381 format, such as "avc1.4d001f".
  rfc6381_codec text not null,

  -- The serialized box, including the leading length and box type (avcC in
  -- the case of H.264).
  data blob not null check (length(data) > 86),

  -- Pixel aspect ratio, if known. As defined in ISO/IEC 14496-12 section
  -- 12.1.4.
  pasp_h_spacing integer not null default 1 check (pasp_h_spacing > 0),
  pasp_v_spacing integer not null default 1 check (pasp_v_spacing > 0)
);

create table user (
  id integer primary key,
  username unique not null,

  -- Bitwise mask of flags:
  -- 1: disabled. If set, no method of authentication for this user will succeed.
  flags integer not null,

  -- If set, a hash for password authentication, as generated by `libpasta::hash_password`.
  password_hash text,

  -- A counter which increments with every password reset or clear.
  password_id integer not null default 0,

  -- Updated lazily on database flush; reset when password_id is incremented.
  -- This could be used to automatically disable the password on hitting a threshold.
  password_failure_count integer not null default 0,

  -- If set, a Unix UID that is accepted for authentication when using HTTP over
  -- a Unix domain socket. (Additionally, the UID running Moonfire NVR can authenticate
  -- as anyone; there's no point in trying to do otherwise.) This might be an easy
  -- bootstrap method once configuration happens through a web UI rather than text UI.
  unix_uid integer,

  -- Permissions available for newly created tokens or when authenticating via
  -- unix_uid above. A serialized "Permissions" protobuf.
  permissions blob not null default X'',

  -- Preferences controlled by the user. A JSON object, or null to represent
  -- the empty object. Can be returned and modified through the API.
  preferences text
);

-- A single session, whether for browser or robot use.
-- These map at the HTTP layer to an "s" cookie (exact format described
-- elsewhere), which holds the session id and an encrypted sequence number for
-- replay protection.
create table user_session (
  -- The session id is a 48-byte blob. This is the unsalted Blake3 (32 bytes)
  -- of the unencoded session id. Much like `password_hash`, a hash is used here
  -- so that a leaked database backup can't be trivially used to steal
  -- credentials.
  session_id_hash blob primary key not null,

  user_id integer references user (id) not null,

  -- A 32-byte random number. Used to derive keys for the replay protection
  -- and CSRF tokens.
  seed blob not null,

  -- A bitwise mask of flags, currently all properties of the HTTP cookie
  -- used to hold the session:
  -- 1: HttpOnly
  -- 2: Secure
  -- 4: SameSite=Lax
  -- 8: SameSite=Strict - 4 must also be set.
  flags integer not null,

  -- The domain of the HTTP cookie used to store this session. The outbound
  -- `Set-Cookie` header never specifies a scope, so this matches the `Host:` of
  -- the inbound HTTP request (minus the :port, if any was specified).
  domain text,

  -- An editable description which might describe the device/program which uses
  -- this session, such as "Chromebook", "iPhone", or "motion detection worker".
  description text,

  creation_password_id integer,        -- the id it was created from, if created via password
  creation_time_sec integer not null,  -- sec since epoch
  creation_user_agent text,            -- User-Agent header from inbound HTTP request.
  creation_peer_addr blob,             -- IPv4 or IPv6 address, or null for Unix socket.

  revocation_time_sec integer,         -- sec since epoch
  revocation_user_agent text,          -- User-Agent header from inbound HTTP request.
  revocation_peer_addr blob,           -- IPv4 or IPv6 address, or null for Unix socket/no peer.

  -- A value indicating the reason for revocation, with optional additional
  -- text detail. Enumeration values:
  -- 0: logout link clicked (i.e. from within the session itself)
  -- 1: obsoleted by a change in hashing algorithm (eg schema 5->6 upgrade)
  --
  -- This might be extended for a variety of other reasons:
  -- x: user revoked (while authenticated in another way)
  -- x: password change invalidated all sessions created with that password
  -- x: expired (due to fixed total time or time inactive)
  -- x: evicted (due to too many sessions)
  -- x: suspicious activity
  revocation_reason integer,
  revocation_reason_detail text,

  -- Information about requests which used this session, updated lazily on database flush.
  last_use_time_sec integer,           -- sec since epoch
  last_use_user_agent text,            -- User-Agent header from inbound HTTP request.
  last_use_peer_addr blob,             -- IPv4 or IPv6 address, or null for Unix socket.
  use_count not null default 0,

  -- Permissions associated with this token; a serialized "Permissions" protobuf.
  permissions blob not null default X''
) without rowid;

create index user_session_uid on user_session (user_id);

-- Timeseries with an enum value, eg:
-- *   camera motion detection results (unknown, still, moving)
-- *   security system arm status (unknown, disarmed, away, stay)
-- *   security system zone status (unknown, normal, violated, trouble)
create table signal (
  id integer primary key,

  -- a uuid describing the originating object, such as the uuid of the camera
  -- for built-in motion detection. There will be a JSON interface for adding
  -- events; it will require this UUID to be supplied. An external uuid might
  -- indicate "my house security system's zone 23".
  source_uuid blob not null check (length(source_uuid) = 16),

  -- a uuid describing the type of event. A registry (TBD) will list built-in
  -- supported types, such as "Hikvision on-camera motion detection", or
  -- "ONVIF on-camera motion detection". External programs can use their own
  -- uuids, such as "Elk security system watcher".
  type_uuid blob not null check (length(type_uuid) = 16),

  -- a short human-readable description to use in mouseovers or event lists,
  -- such as "driveway motion" or "front door open".
  short_name not null,

  unique (source_uuid, type_uuid)
);

-- e.g. "still/moving", "disarmed/away/stay", etc.
create table signal_type_enum (
  type_uuid blob not null check (length(type_uuid) = 16),
  value integer not null check (value > 0 and value < 16),
  name text not null,

  -- true/1 iff this signal value should be considered "motion" for directly associated cameras.
  motion int not null check (motion in (0, 1)) default 0,

  color text
);

-- Associations between event sources and cameras.
-- For example, if two cameras have overlapping fields of view, they might be
-- configured such that each camera is associated with both its own motion and
-- the other camera's motion.
create table signal_camera (
  signal_id integer references signal (id),
  camera_id integer references camera (id),

  -- type:
  --
  -- 0 means direct association, as if the event source if the camera's own
  -- motion detection. Here are a couple ways this could be used:
  --
  -- * when viewing the camera, hotkeys to go to the start of the next or
  --   previous event should respect this event.
  -- * a list of events might include the recordings associated with the
  --   camera in the same timespan.
  --
  -- 1 means indirect association. A screen associated with the camera should
  -- given some indication of this event, but there should be no assumption
  -- that the camera will have a direct view of the event. For example, all
  -- cameras might be indirectly associated with a doorknob press. Cameras at
  -- the back of the house shouldn't be expected to have a direct view of this
  -- event, but motion events shortly afterward might warrant extra scrutiny.
  type integer not null,

  primary key (signal_id, camera_id)
) without rowid;

-- Changes to signals as of a given timestamp.
create table signal_change (
  -- Event time, in 90 kHz units since 1970-01-01 00:00:00Z excluding leap seconds.
  time_90k integer primary key,

  -- Changes at this timestamp.
  --
  -- A blob of varints representing a list of
  -- (signal number - next allowed, state) pairs, where signal number is
  -- non-decreasing. For example,
  -- input signals: 1         3         200 (must be sorted)
  -- delta:         1         1         196 (must be non-negative)
  -- states:             1         1              2
  -- varint:        \x01 \x01 \x01 \x01 \xc4 \x01 \x02
  changes blob not null
);

insert into version (id, unix_time,                           notes)
             values (7,  cast(strftime('%s', 'now') as int), 'db creation');
