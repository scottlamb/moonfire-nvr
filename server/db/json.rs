// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! JSON types for use in the database schema. See references from `schema.sql`.

use rusqlite::types::{FromSqlError, ValueRef};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;

macro_rules! sql {
    ($l:ident) => {
        impl rusqlite::types::FromSql for $l {
            fn column_result(value: ValueRef) -> Result<Self, FromSqlError> {
                match value {
                    ValueRef::Text(t) => {
                        Ok(serde_json::from_slice(t)
                            .map_err(|e| FromSqlError::Other(Box::new(e)))?)
                    }
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CameraConfig {
    /// A short description of the camera.
    pub description: String,

    /// The base URL for accessing ONVIF; `device_service` will be joined on
    /// automatically to form the device management service URL.
    /// Eg with `onvif_base=http://192.168.1.110:85`, the full
    /// URL of the devie management service will be
    /// `http://192.168.1.110:85/device_service`.
    pub onvif_base_url: Option<Url>,

    /// The username to use when accessing the camera.
    /// If empty, no username or password will be supplied.
    pub username: String,

    /// The password to use when accessing the camera.
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

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamConfig {
    /// The mode of operation for this camera on startup.
    ///
    /// Null means entirely disabled. At present, so does any value other than
    /// `record`.
    #[serde(default)]
    pub mode: String,

    /// The `rtsp://` URL to use for this stream, excluding username and
    /// password.
    ///
    /// In the future, this might support additional protocols such as `rtmp://`
    /// or even a private use URI scheme for the [Baichuan
    /// protocol](https://github.com/thirtythreeforty/neolink).
    ///
    /// (Credentials are taken from [`CameraConfig`]'s respective fields.)
    // TODO: should this really be Option?
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
