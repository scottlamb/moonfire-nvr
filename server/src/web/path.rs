// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2021 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception.

//! Decodes request paths.

use std::str::FromStr;
use uuid::Uuid;

/// A decoded request path.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum Path {
    TopLevel,                                         // "/api/"
    Request,                                          // "/api/request"
    InitSegment(i32, bool),                           // "/api/init/<id>.mp4{.txt}"
    Camera(Uuid),                                     // "/api/cameras/<uuid>/"
    Signals,                                          // "/api/signals"
    StreamRecordings(Uuid, db::StreamType),           // "/api/cameras/<uuid>/<type>/recordings"
    StreamViewMp4(Uuid, db::StreamType, bool),        // "/api/cameras/<uuid>/<type>/view.mp4{.txt}"
    StreamViewMp4Segment(Uuid, db::StreamType, bool), // "/api/cameras/<uuid>/<type>/view.m4s{.txt}"
    StreamLiveMp4Segments(Uuid, db::StreamType),      // "/api/cameras/<uuid>/<type>/live.m4s"
    Login,                                            // "/api/login"
    Logout,                                           // "/api/logout"
    Static,                                           // (anything that doesn't start with "/api/")
    User(i32),                                        // "/api/users/<id>"
    NotFound,
}

impl Path {
    /// Decodes a request path, notably not including any request parameters.
    pub(super) fn decode(path: &str) -> Self {
        let path = match path.strip_prefix("/api/") {
            Some(p) => p,
            None => return Path::Static,
        };
        match path {
            "" => return Path::TopLevel,
            "login" => return Path::Login,
            "logout" => return Path::Logout,
            "request" => return Path::Request,
            "signals" => return Path::Signals,
            _ => {}
        };
        if let Some(path) = path.strip_prefix("init/") {
            let (debug, path) = match path.strip_suffix(".txt") {
                Some(p) => (true, p),
                None => (false, path),
            };
            let path = match path.strip_suffix(".mp4") {
                Some(p) => p,
                None => return Path::NotFound,
            };
            if let Ok(id) = i32::from_str(path) {
                return Path::InitSegment(id, debug);
            }
            Path::NotFound
        } else if let Some(path) = path.strip_prefix("cameras/") {
            let (uuid, path) = match path.split_once('/') {
                Some(pair) => pair,
                None => return Path::NotFound,
            };

            // TODO(slamb): require uuid to be in canonical format.
            let uuid = match Uuid::parse_str(uuid) {
                Ok(u) => u,
                Err(_) => return Path::NotFound,
            };

            if path.is_empty() {
                return Path::Camera(uuid);
            }

            let (type_, path) = match path.split_once('/') {
                Some(pair) => pair,
                None => return Path::NotFound,
            };
            let type_ = match db::StreamType::parse(type_) {
                None => {
                    return Path::NotFound;
                }
                Some(t) => t,
            };
            match path {
                "recordings" => Path::StreamRecordings(uuid, type_),
                "view.mp4" => Path::StreamViewMp4(uuid, type_, false),
                "view.mp4.txt" => Path::StreamViewMp4(uuid, type_, true),
                "view.m4s" => Path::StreamViewMp4Segment(uuid, type_, false),
                "view.m4s.txt" => Path::StreamViewMp4Segment(uuid, type_, true),
                "live.m4s" => Path::StreamLiveMp4Segments(uuid, type_),
                _ => Path::NotFound,
            }
        } else if let Some(path) = path.strip_prefix("users/") {
            if let Ok(id) = i32::from_str(path) {
                return Path::User(id);
            }
            Path::NotFound
        } else {
            Path::NotFound
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn paths() {
        use super::Path;
        use uuid::Uuid;
        let cam_uuid = Uuid::parse_str("35144640-ff1e-4619-b0d5-4c74c185741c").unwrap();
        assert_eq!(Path::decode("/foo"), Path::Static);
        assert_eq!(Path::decode("/api/"), Path::TopLevel);
        assert_eq!(
            Path::decode("/api/init/42.mp4"),
            Path::InitSegment(42, false)
        );
        assert_eq!(
            Path::decode("/api/init/42.mp4.txt"),
            Path::InitSegment(42, true)
        );
        assert_eq!(Path::decode("/api/init/x.mp4"), Path::NotFound); // non-digit
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/"),
            Path::Camera(cam_uuid)
        );
        assert_eq!(Path::decode("/api/cameras/asdf/"), Path::NotFound);
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::Main)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/sub/recordings"),
            Path::StreamRecordings(cam_uuid, db::StreamType::Sub)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/junk/recordings"),
            Path::NotFound
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::Main, false)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.mp4.txt"),
            Path::StreamViewMp4(cam_uuid, db::StreamType::Main, true)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::Main, false)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/view.m4s.txt"),
            Path::StreamViewMp4Segment(cam_uuid, db::StreamType::Main, true)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/live.m4s"),
            Path::StreamLiveMp4Segments(cam_uuid, db::StreamType::Main)
        );
        assert_eq!(
            Path::decode("/api/cameras/35144640-ff1e-4619-b0d5-4c74c185741c/main/junk"),
            Path::NotFound
        );
        assert_eq!(Path::decode("/api/login"), Path::Login);
        assert_eq!(Path::decode("/api/logout"), Path::Logout);
        assert_eq!(Path::decode("/api/signals"), Path::Signals);
        assert_eq!(Path::decode("/api/junk"), Path::NotFound);
        assert_eq!(Path::decode("/api/users/42"), Path::User(42));
        assert_eq!(Path::decode("/api/users/asdf"), Path::NotFound);
    }
}
