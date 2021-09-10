// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2020 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

use base::time::{Duration, Time};
use db::auth::SessionHash;
use failure::{format_err, Error};
use serde::ser::{Error as _, SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};
use std::ops::Not;
use uuid::Uuid;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TopLevel<'a> {
    pub time_zone_name: &'a str,

    // Use a custom serializer which presents the map's values as a sequence and includes the
    // "days" and "camera_configs" attributes or not, according to the respective bools.
    #[serde(serialize_with = "TopLevel::serialize_cameras")]
    pub cameras: (&'a db::LockedDatabase, bool, bool),

    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<ToplevelUser>,

    #[serde(serialize_with = "TopLevel::serialize_signals")]
    pub signals: (&'a db::LockedDatabase, bool),

    #[serde(serialize_with = "TopLevel::serialize_signal_types")]
    pub signal_types: &'a db::LockedDatabase,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(serialize_with = "Session::serialize_csrf")]
    pub csrf: SessionHash,
}

impl Session {
    fn serialize_csrf<S>(csrf: &SessionHash, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut tmp = [0u8; 32];
        csrf.encode_base64(&mut tmp);
        serializer.serialize_str(::std::str::from_utf8(&tmp[..]).expect("base64 is UTF-8"))
    }
}

/// JSON serialization wrapper for a single camera when processing `/api/` and
/// `/api/cameras/<uuid>/`. See `design/api.md` for details.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Camera<'a> {
    pub uuid: Uuid,
    pub id: i32,
    pub short_name: &'a str,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<&'a db::json::CameraConfig>,

    #[serde(serialize_with = "Camera::serialize_streams")]
    pub streams: [Option<Stream<'a>>; 2],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraConfig<'a> {
    pub onvif_host: &'a str,
    pub username: Option<&'a str>,
    pub password: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Stream<'a> {
    pub id: i32,
    pub retain_bytes: i64,
    pub min_start_time_90k: Option<Time>,
    pub max_end_time_90k: Option<Time>,
    pub total_duration_90k: Duration,
    pub total_sample_file_bytes: i64,
    pub fs_bytes: i64,
    pub record: bool,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "Stream::serialize_days")]
    pub days: Option<db::days::Map<db::days::StreamValue>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<&'a db::json::StreamConfig>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signal<'a> {
    pub id: u32,
    #[serde(serialize_with = "Signal::serialize_cameras")]
    pub cameras: (&'a db::Signal, &'a db::LockedDatabase),
    pub source: Uuid,
    pub type_: Uuid,
    pub short_name: &'a str,

    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(serialize_with = "Signal::serialize_days")]
    pub days: Option<&'a db::days::Map<db::days::SignalValue>>,
}

#[derive(Deserialize)]
#[serde(tag = "base", content = "rel90k", rename_all = "camelCase")]
pub enum PostSignalsTimeBase {
    Epoch(Time),
    Now(Duration),
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest<'a> {
    pub username: &'a str,
    pub password: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogoutRequest<'a> {
    pub csrf: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostSignalsRequest {
    pub signal_ids: Vec<u32>,
    pub states: Vec<u16>,
    pub start: PostSignalsTimeBase,
    pub end: PostSignalsTimeBase,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostSignalsResponse {
    pub time_90k: Time,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Signals {
    pub times_90k: Vec<Time>,
    pub signal_ids: Vec<u32>,
    pub states: Vec<u16>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalType<'a> {
    pub uuid: Uuid,

    #[serde(serialize_with = "SignalType::serialize_states")]
    pub states: &'a db::signal::Type,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalTypeState<'a> {
    value: u16,
    name: &'a str,

    #[serde(skip_serializing_if = "Not::not")]
    motion: bool,
    color: &'a str,
}

impl<'a> Camera<'a> {
    pub fn wrap(
        c: &'a db::Camera,
        db: &'a db::LockedDatabase,
        include_days: bool,
        include_config: bool,
    ) -> Result<Self, Error> {
        Ok(Camera {
            uuid: c.uuid,
            id: c.id,
            short_name: &c.short_name,
            config: match include_config {
                false => None,
                true => Some(&c.config),
            },
            streams: [
                Stream::wrap(db, c.streams[0], include_days, include_config)?,
                Stream::wrap(db, c.streams[1], include_days, include_config)?,
            ],
        })
    }

    fn serialize_streams<S>(streams: &[Option<Stream>; 2], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(streams.len()))?;
        for (i, s) in streams.iter().enumerate() {
            if let Some(ref s) = *s {
                map.serialize_key(
                    db::StreamType::from_index(i)
                        .expect("invalid stream type index")
                        .as_str(),
                )?;
                map.serialize_value(s)?;
            }
        }
        map.end()
    }
}

impl<'a> Stream<'a> {
    fn wrap(
        db: &'a db::LockedDatabase,
        id: Option<i32>,
        include_days: bool,
        include_config: bool,
    ) -> Result<Option<Self>, Error> {
        let id = match id {
            Some(id) => id,
            None => return Ok(None),
        };
        let s = db
            .streams_by_id()
            .get(&id)
            .ok_or_else(|| format_err!("missing stream {}", id))?;
        Ok(Some(Stream {
            id: s.id,
            retain_bytes: s.config.retain_bytes,
            min_start_time_90k: s.range.as_ref().map(|r| r.start),
            max_end_time_90k: s.range.as_ref().map(|r| r.end),
            total_duration_90k: s.duration,
            total_sample_file_bytes: s.sample_file_bytes,
            fs_bytes: s.fs_bytes,
            record: s.config.mode == db::json::STREAM_MODE_RECORD,
            days: if include_days { Some(s.days()) } else { None },
            config: match include_config {
                false => None,
                true => Some(&s.config),
            },
        }))
    }

    fn serialize_days<S>(
        days: &Option<db::days::Map<db::days::StreamValue>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let days = match days.as_ref() {
            Some(d) => d,
            None => return serializer.serialize_none(),
        };
        let mut map = serializer.serialize_map(Some(days.len()))?;
        for (k, v) in days {
            map.serialize_key(k.as_ref())?;
            let bounds = k.bounds();
            map.serialize_value(&StreamDayValue {
                start_time_90k: bounds.start,
                end_time_90k: bounds.end,
                total_duration_90k: v.duration,
            })?;
        }
        map.end()
    }
}

impl<'a> Signal<'a> {
    pub fn wrap(s: &'a db::Signal, db: &'a db::LockedDatabase, include_days: bool) -> Self {
        Signal {
            id: s.id,
            cameras: (s, db),
            source: s.source,
            type_: s.type_,
            short_name: &s.short_name,
            days: if include_days { Some(&s.days) } else { None },
        }
    }

    fn serialize_cameras<S>(
        cameras: &(&db::Signal, &db::LockedDatabase),
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (s, db) = cameras;
        let mut map = serializer.serialize_map(Some(s.cameras.len()))?;
        for sc in &s.cameras {
            let c = db.cameras_by_id().get(&sc.camera_id).ok_or_else(|| {
                S::Error::custom(format!("signal has missing camera id {}", sc.camera_id))
            })?;
            map.serialize_key(&c.uuid)?;
            map.serialize_value(match sc.type_ {
                db::signal::SignalCameraType::Direct => "direct",
                db::signal::SignalCameraType::Indirect => "indirect",
            })?;
        }
        map.end()
    }

    fn serialize_days<S>(
        days: &Option<&db::days::Map<db::days::SignalValue>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let days = match *days {
            Some(d) => d,
            None => return serializer.serialize_none(),
        };
        let mut map = serializer.serialize_map(Some(days.len()))?;
        for (k, v) in days {
            map.serialize_key(k.as_ref())?;
            let bounds = k.bounds();
            map.serialize_value(&SignalDayValue {
                start_time_90k: bounds.start,
                end_time_90k: bounds.end,
                states: &v.states[..],
            })?;
        }
        map.end()
    }
}

impl<'a> SignalType<'a> {
    pub fn wrap(uuid: Uuid, type_: &'a db::signal::Type) -> Self {
        SignalType {
            uuid,
            states: type_,
        }
    }

    fn serialize_states<S>(type_: &db::signal::Type, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut seq = serializer.serialize_seq(Some(type_.states.len()))?;
        for s in &type_.states {
            seq.serialize_element(&SignalTypeState::wrap(s))?;
        }
        seq.end()
    }
}

impl<'a> SignalTypeState<'a> {
    pub fn wrap(s: &'a db::signal::TypeState) -> Self {
        SignalTypeState {
            value: s.value,
            name: &s.name,
            motion: s.motion,
            color: &s.color,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct StreamDayValue {
    pub start_time_90k: Time,
    pub end_time_90k: Time,
    pub total_duration_90k: Duration,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SignalDayValue<'a> {
    pub start_time_90k: Time,
    pub end_time_90k: Time,
    pub states: &'a [u64],
}

impl<'a> TopLevel<'a> {
    /// Serializes cameras as a list (rather than a map), optionally including the `days` and
    /// `cameras` fields.
    fn serialize_cameras<S>(
        cameras: &(&db::LockedDatabase, bool, bool),
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (db, include_days, include_config) = *cameras;
        let cs = db.cameras_by_id();
        let mut seq = serializer.serialize_seq(Some(cs.len()))?;
        for c in cs.values() {
            seq.serialize_element(
                &Camera::wrap(c, db, include_days, include_config).map_err(S::Error::custom)?,
            )?;
        }
        seq.end()
    }

    /// Serializes signals as a list (rather than a map), optionally including the `days` field.
    fn serialize_signals<S>(
        signals: &(&db::LockedDatabase, bool),
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (db, include_days) = *signals;
        let ss = db.signals_by_id();
        let mut seq = serializer.serialize_seq(Some(ss.len()))?;
        for s in ss.values() {
            seq.serialize_element(&Signal::wrap(s, db, include_days))?;
        }
        seq.end()
    }

    /// Serializes signals as a list (rather than a map), optionally including the `days` field.
    fn serialize_signal_types<S>(db: &db::LockedDatabase, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let ss = db.signal_types_by_uuid();
        let mut seq = serializer.serialize_seq(Some(ss.len()))?;
        for (u, t) in ss {
            seq.serialize_element(&SignalType::wrap(*u, t))?;
        }
        seq.end()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListRecordings<'a> {
    pub recordings: Vec<Recording>,

    // There are likely very few video sample entries for a given stream in a given day, so
    // representing with an unordered Vec (and having O(n) insert-if-absent) is probably better
    // than dealing with a HashSet's code bloat.
    #[serde(serialize_with = "ListRecordings::serialize_video_sample_entries")]
    pub video_sample_entries: (&'a db::LockedDatabase, Vec<i32>),
}

impl<'a> ListRecordings<'a> {
    fn serialize_video_sample_entries<S>(
        video_sample_entries: &(&db::LockedDatabase, Vec<i32>),
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let (db, ref v) = *video_sample_entries;
        let mut map = serializer.serialize_map(Some(v.len()))?;
        for id in v {
            map.serialize_entry(
                id,
                &VideoSampleEntry::from(&db.video_sample_entries_by_id().get(id).unwrap()),
            )?;
        }
        map.end()
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recording {
    pub start_time_90k: i64,
    pub end_time_90k: i64,
    pub sample_file_bytes: i64,
    pub video_samples: i64,
    pub video_sample_entry_id: i32,
    pub start_id: i32,
    pub open_id: u32,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_uncommitted: Option<i32>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_id: Option<i32>,

    #[serde(skip_serializing_if = "Not::not")]
    pub growing: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSampleEntry {
    pub width: u16,
    pub height: u16,
    pub pasp_h_spacing: u16,
    pub pasp_v_spacing: u16,
    pub aspect_width: u32,
    pub aspect_height: u32,
}

impl VideoSampleEntry {
    fn from(e: &db::VideoSampleEntry) -> Self {
        let aspect = e.aspect();
        Self {
            width: e.width,
            height: e.height,
            pasp_h_spacing: e.pasp_h_spacing,
            pasp_v_spacing: e.pasp_v_spacing,
            aspect_width: *aspect.numer(),
            aspect_height: *aspect.denom(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToplevelUser {
    pub name: String,
    pub id: i32,
    pub preferences: db::auth::UserPreferences,
    pub session: Option<Session>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostUser {
    pub update: Option<UserSubset>,
    pub precondition: Option<UserSubset>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserSubset {
    pub preferences: Option<db::auth::UserPreferences>,
}
