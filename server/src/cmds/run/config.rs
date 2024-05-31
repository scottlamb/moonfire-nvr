// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2022 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Runtime configuration file (`/etc/moonfire-nvr.toml`).
//! See `ref/config.md` for more description.

use std::path::PathBuf;

use serde::Deserialize;

use crate::json::Permissions;

fn default_db_dir() -> PathBuf {
    crate::DEFAULT_DB_DIR.into()
}

/// Top-level configuration file object.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct ConfigFile {
    pub binds: Vec<BindConfig>,

    /// Directory holding the SQLite3 index database.
    ///
    /// default: `/var/lib/moonfire-nvr/db`.
    #[serde(default = "default_db_dir")]
    pub db_dir: PathBuf,

    /// Directory holding user interface files (`.html`, `.js`, etc).
    #[cfg_attr(not(feature = "bundled-ui"), serde(default))]
    #[cfg_attr(feature = "bundled-ui", serde(default))]
    pub ui_dir: UiDir,

    /// The number of worker threads used by the asynchronous runtime.
    ///
    /// Defaults to the number of cores on the system.
    #[serde(default)]
    pub worker_threads: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", untagged)]
pub enum UiDir {
    FromFilesystem(PathBuf),
    Bundled(#[allow(unused)] BundledUi),
}

impl Default for UiDir {
    #[cfg(feature = "bundled-ui")]
    fn default() -> Self {
        UiDir::Bundled(BundledUi { bundled: true })
    }

    #[cfg(not(feature = "bundled-ui"))]
    fn default() -> Self {
        UiDir::FromFilesystem("/usr/local/lib/moonfire-nvr/ui".into())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct BundledUi {
    /// Just a marker to select this variant.
    #[allow(unused)]
    bundled: bool,
}

/// Per-bind configuration.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct BindConfig {
    /// The address to bind to.
    #[serde(flatten)]
    pub address: AddressConfig,

    /// Allow unauthenticated API access on this bind, with the given
    /// permissions (defaults to empty).
    ///
    /// Note that even an empty string allows some basic access that would be rejected if the
    /// argument were omitted.
    #[serde(default)]
    pub allow_unauthenticated_permissions: Option<Permissions>,

    /// Trusts `X-Real-IP:` and `X-Forwarded-Proto:` headers on the incoming request.
    ///
    /// Set this only after ensuring your proxy server is configured to set them
    /// and that no untrusted requests bypass the proxy server. You may want to
    /// specify a localhost bind address.
    #[serde(default)]
    pub trust_forward_headers: bool,

    /// On Unix-domain sockets, treat clients with the Moonfire NVR server's own
    /// effective UID as privileged.
    #[serde(default)]
    pub own_uid_is_privileged: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub enum AddressConfig {
    /// IPv4 address such as `0.0.0.0:8080` or `127.0.0.1:8080`.
    Ipv4(std::net::SocketAddrV4),

    /// IPv6 address such as `[::]:8080` or `[::1]:8080`.
    Ipv6(std::net::SocketAddrV6),

    /// Unix socket path such as `/var/lib/moonfire-nvr/sock`.
    Unix(PathBuf),

    /// `systemd` socket activation.
    ///
    /// See [systemd.socket(5) manual
    /// page](https://www.freedesktop.org/software/systemd/man/systemd.socket.html).
    Systemd(#[cfg_attr(not(target_os = "linux"), allow(unused))] String),
}
