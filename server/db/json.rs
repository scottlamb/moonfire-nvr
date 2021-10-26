// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! JSON types for use in the database schema. See references from `schema.sql`.
//!
//! In general, every table in the database with reasonably low expected row
//! count should have a JSON config column. This allows the schema to be
//! modified without a major migration.
//!
//! JSON should be avoided for very high-row-count tables (eg `reocrding`) for
//! storage efficiency, in favor of separate columns or a binary type.
//! (Currently protobuf is used within `user_session`. A future schema version
//! might switch to a more JSON-like binary format to minimize impedance
//! mismatch.)
//!
//! JSON types should be designed for extensibility with forward and backward
//! compatibility:
//!
//! *   Every struct has a flattened `unknown` so that if an unknown attribute is
//!     written with a newer version of the binary, then the config is saved
//!     (read and re-written) with an older version, the value will be
//!     preserved.
//! *   If a field is only for use by the UI and there's no need for the server
//!     to constrain it, leave it in `unknown`.
//! *   Fields shouldn't use closed enumerations or other restrictive types,
//!     so that parsing the config with a non-understood value will not fail. If
//!     the behavior of unknown values is not obvious, it should be clarified
//!     via a comment.
//! *   Fields should generally parse without values, via `#[serde(default)]`,
//!     so that they can be removed in a future version if they no longer make
//!     sense. It also makes sense to avoid serializing them when empty.

use std::{collections::BTreeMap, path::PathBuf};

use rusqlite::types::{FromSqlError, ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;
use uuid::Uuid;

/// Serializes and deserializes JSON as a SQLite3 `text` column, compatible with the
/// [JSON1 extension](https://www.sqlite.org/json1.html).
macro_rules! sql {
    ($l:ident) => {
        impl rusqlite::types::FromSql for $l {
            fn column_result(value: ValueRef) -> Result<Self, FromSqlError> {
                match value {
                    ValueRef::Text(t) => {
                        Ok(serde_json::from_slice(t)
                            .map_err(|e| FromSqlError::Other(Box::new(e)))?)
                    }
                    ValueRef::Null => Ok($l::default()),
                    _ => Err(FromSqlError::InvalidType),
                }
            }
        }

        impl rusqlite::types::ToSql for $l {
            fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
                Ok(serde_json::to_string(&self)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(e.into()))?
                    .into())
            }
        }
    };
}

/// Global configuration, used in the `config` column of the `meta` table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalConfig {
    /// The maximum number of entries in the `signal_state` table (or `None` for unlimited).
    ///
    /// If an update causes this to be exceeded, older times will be garbage
    /// collected to stay within the limit.
    pub max_signal_changes: Option<u32>,

    /// Information about signal types.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub signal_types: BTreeMap<Uuid, SignalTypeConfig>,

    /// Information about signals.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub signals: BTreeMap<u32, SignalConfig>,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(GlobalConfig);

/// Sample file directory configuration, used in the `config` column of the `sample_file_dir` table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleFileDirConfig {
    pub path: PathBuf,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(SampleFileDirConfig);

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalTypeConfig {
    /// Information about possible enumeration values of this signal type.
    ///
    /// 0 always means `unknown`. Other values may be specified here to set
    /// their configuration. It's more efficient in terms of encoded length
    /// and RAM at runtime for common values (eg, `still`, `normal`, or
    /// `disarmed`) to be numerically lower than rarer values (eg `motion`,
    /// `violated`, or `armed`) and for the value space to be dense (eg, to use
    /// values 1, 2, 3 rather than 1, 10, 20).
    ///
    /// Currently values must be in the range `[0, 16)`.
    ///
    /// Nothing enforces that only values specified here may be set for a signal
    /// of this type.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub values: BTreeMap<u8, SignalTypeValueConfig>,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(SignalTypeConfig);

/// Information about a signal type value; used in `SignalTypeConfig::values`.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalTypeValueConfig {
    pub name: String,

    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub motion: bool,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub color: String,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}

impl SignalTypeValueConfig {
    pub fn is_empty(&self) -> bool {
        self.unknown.is_empty()
    }
}

/// Camera configuration, used in the `config` column of the `camera` table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraConfig {
    /// A short description of the camera.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,

    /// The base URL for accessing ONVIF; `device_service` will be joined on
    /// automatically to form the device management service URL.
    /// Eg with `onvif_base=http://192.168.1.110:85`, the full
    /// URL of the device management service will be
    /// `http://192.168.1.110:85/device_service`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub onvif_base_url: Option<Url>,

    /// The username to use when accessing the camera.
    /// If empty, no username or password will be supplied.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub username: String,

    /// The password to use when accessing the camera.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(CameraConfig);

impl CameraConfig {
    pub fn is_empty(&self) -> bool {
        self.description.is_empty()
            && self.onvif_base_url.is_none()
            && self.username.is_empty()
            && self.password.is_empty()
            && self.unknown.is_empty()
    }
}

/// Stream configuration, used in the `config` column of the `stream` table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamConfig {
    /// The mode of operation for this camera on startup.
    ///
    /// Null means entirely disabled. At present, so does any value other than
    /// `record`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mode: String,

    /// The `rtsp://` URL to use for this stream, excluding username and
    /// password.
    ///
    /// In the future, this might support additional protocols such as `rtmp://`
    /// or even a private use URI scheme for the [Baichuan
    /// protocol](https://github.com/thirtythreeforty/neolink).
    ///
    /// (Credentials are taken from [`CameraConfig`]'s respective fields.)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<Url>,

    /// The number of bytes of video to retain, excluding the
    /// currently-recording file.
    ///
    /// Older files will be deleted as necessary to stay within this limit.
    #[serde(default)]
    pub retain_bytes: i64,

    /// Flush the database when the first instant of completed recording is this
    /// many seconds old. A value of 0 means that every completed recording will
    /// cause an immediate flush. Higher values may allow flushes to be combined,
    /// reducing SSD write cycles. For example, if all streams have a
    /// `flush_if_sec` >= *x* sec, there will be:
    ///
    /// * at most one flush per *x* sec in total
    /// * at most *x* sec of completed but unflushed recordings per stream.
    /// * at most *x* completed but unflushed recordings per stream, in the
    ///   worst case where a recording instantly fails, waits the 1-second retry
    ///   delay, then fails again, forever.
    #[serde(default)]
    pub flush_if_sec: u32,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(StreamConfig);

pub const STREAM_MODE_RECORD: &'static str = "record";

impl StreamConfig {
    pub fn is_empty(&self) -> bool {
        self.mode.is_empty()
            && self.url.is_none()
            && self.retain_bytes == 0
            && self.flush_if_sec == 0
            && self.unknown.is_empty()
    }
}

/// Signal configuration, used in the `config` column of the `signal` table.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalConfig {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub short_name: String,

    /// Map of associated cameras to the type of association.
    ///
    /// `direct` is as if the event source is the camera's own motion detection.
    /// Here are a couple ways this could be used:
    ///
    ///  * when viewing the camera, hotkeys to go to the start of the next or
    ///    previous event should respect this event.
    ///  * a list of events might include the recordings associated with the
    ///    camera in the same timespan.
    ///
    ///  `indirect` might mean a screen associated with the camera should given
    ///  some indication of this event, but there should be no assumption that
    ///  the camera will have a direct view of the event. For example, all
    ///  cameras might be indirectly associated with a doorknob press. Cameras
    ///  at the back of the house shouldn't be expected to have a direct view of
    ///  this event, but motion events shortly afterward might warrant extra
    ///  scrutiny.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub camera_associations: BTreeMap<i32, String>,

    #[serde(flatten)]
    pub unknown: Map<String, Value>,
}
sql!(SignalConfig);
