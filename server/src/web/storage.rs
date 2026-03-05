// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2024 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Storage management API endpoints.

use crate::json;
use base::{bail, err};
use http::{Method, Request, StatusCode};
use std::path::PathBuf;

use super::{
    into_json_body, parse_json_body, plain_response, require_csrf_if_session, serve_json, Caller,
    ResponseResult, Service,
};

impl Service {
    pub(super) async fn storage(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        match *req.method() {
            Method::GET => {
                if !permissions.view_video {
                    bail!(PermissionDenied, msg("view_video required"));
                }
                self.get_storage(&req, caller).await
            }
            Method::POST => {
                if !permissions.admin_cameras {
                    bail!(PermissionDenied, msg("admin_cameras required"));
                }
                self.post_storage(req, caller).await
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET or POST expected",
            )),
        }
    }

    pub(super) async fn storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        match *req.method() {
            Method::GET => {
                if !permissions.view_video {
                    bail!(PermissionDenied, msg("view_video required"));
                }
                self.get_storage_dir(&req, caller, id).await
            }
            Method::PATCH => {
                if !permissions.admin_cameras {
                    bail!(PermissionDenied, msg("admin_cameras required"));
                }
                self.patch_storage_dir(req, caller, id).await
            }
            Method::DELETE => {
                if !permissions.admin_cameras {
                    bail!(PermissionDenied, msg("admin_cameras required"));
                }
                self.delete_storage_dir(req, caller, id).await
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, PATCH, or DELETE expected",
            )),
        }
    }

    /// Collect stream usage information for a given storage directory.
    fn stream_usage_for_dir(
        db: &db::LockedDatabase,
        dir_id: i32,
    ) -> (i64, Vec<json::StorageStreamUsage>) {
        let mut used_bytes = 0i64;
        let mut streams_using = Vec::new();

        for (&stream_id, stream) in db.streams_by_id() {
            let s = stream.inner.lock();
            if s.sample_file_dir.as_ref().map(|d| d.id) == Some(dir_id) {
                used_bytes += s.committed.fs_bytes;
                streams_using.push(json::StorageStreamUsage {
                    stream_id,
                    camera_name: db
                        .cameras_by_id()
                        .get(&s.camera_id)
                        .expect("stream's camera should exist")
                        .short_name
                        .clone(),
                    stream_type: s.type_.as_str().to_string(),
                    used_bytes: s.committed.fs_bytes,
                    duration_90k: s.committed.duration.0,
                });
            }
        }

        (used_bytes, streams_using)
    }

    async fn get_storage(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
    ) -> ResponseResult {
        // Collect dir info while holding the db lock, then release before async statfs.
        let mut storage_dirs: Vec<(json::StorageDir, db::dir::Pool)>;
        {
            let db = self.db.lock();
            storage_dirs = Vec::new();

            for (&id, dir) in db.sample_file_dirs_by_id() {
                let (used_bytes, streams_using) = Self::stream_usage_for_dir(&db, id);
                let pool = dir.pool().clone();
                storage_dirs.push((
                    json::StorageDir {
                        id,
                        uuid: pool.uuid(),
                        path: pool.path().to_path_buf(),
                        total_bytes: None,
                        used_bytes,
                        streams_using,
                    },
                    pool,
                ));
            }
        }

        // Fetch filesystem stats asynchronously.
        for (dir, pool) in &mut storage_dirs {
            if let Ok(stat) = pool.run("statfs", |ctx| ctx.statfs()).await {
                #[allow(clippy::useless_conversion)]
                let bytes = u64::from(stat.blocks_available()) * u64::from(stat.fragment_size());
                dir.total_bytes = Some(bytes as i64);
            }
        }

        let dirs: Vec<json::StorageDir> = storage_dirs.into_iter().map(|(d, _)| d).collect();
        serve_json(req, &json::GetStorageResponse { storage_dirs: dirs })
    }

    async fn get_storage_dir(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let (mut storage_dir, pool) = {
            let db = self.db.lock();
            let dir = db
                .sample_file_dirs_by_id()
                .get(&id)
                .ok_or_else(|| err!(NotFound, msg("no such storage directory {id}")))?;

            let (used_bytes, streams_using) = Self::stream_usage_for_dir(&db, id);
            let pool = dir.pool().clone();
            (
                json::StorageDir {
                    id,
                    uuid: pool.uuid(),
                    path: pool.path().to_path_buf(),
                    total_bytes: None,
                    used_bytes,
                    streams_using,
                },
                pool,
            )
        };

        if let Ok(stat) = pool.run("statfs", |ctx| ctx.statfs()).await {
            #[allow(clippy::useless_conversion)]
            let bytes = u64::from(stat.blocks_available()) * u64::from(stat.fragment_size());
            storage_dir.total_bytes = Some(bytes as i64);
        }

        serve_json(req, &storage_dir)
    }

    async fn post_storage(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::PostStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        let id = self.db.add_sample_file_dir(PathBuf::from(r.path)).await?;

        // Get the UUID from the created directory
        let uuid = self
            .db
            .lock()
            .sample_file_dirs_by_id()
            .get(&id)
            .ok_or_else(|| err!(Internal, msg("directory not found after creation")))?
            .pool()
            .uuid();

        serve_json(&parts, &json::PostStorageResponse { id, uuid })
    }

    async fn patch_storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        _id: i32,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::PatchStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        // Storage directories can't be updated yet; this endpoint exists for
        // future extensibility.
        serve_json(&parts, &json::EmptyResponse {})
    }

    async fn delete_storage_dir(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let (parts, b) = into_json_body(req).await?;
        let r: json::DeleteStorageRequest = parse_json_body(&b)?;
        require_csrf_if_session(&caller, r.csrf)?;

        self.db.delete_sample_file_dir(id).await?;

        serve_json(&parts, &json::EmptyResponse {})
    }

    pub(super) fn storage_dirs_simple(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        if !permissions.view_video {
            bail!(PermissionDenied, msg("view_video required"));
        }

        let db = self.db.lock();
        let mut dirs = Vec::new();

        for (&id, dir) in db.sample_file_dirs_by_id() {
            dirs.push(json::StorageDirSimple {
                id,
                path: dir.pool().path().to_path_buf(),
            });
        }

        serve_json(&req, &json::GetStorageDirsSimpleResponse { dirs })
    }
}

#[cfg(test)]
mod tests {
    use crate::web::tests::Server;
    use db::testutil;
    use http::{Method, StatusCode};
    use serde_json::json;
    use tempfile::TempDir;

    async fn make_request(
        server: &Server,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> reqwest::Response {
        let client = reqwest::Client::new();
        let url = format!("{}/api{}", server.base_url, path);

        let mut req = match method {
            Method::GET => client.get(&url),
            Method::POST => client.post(&url),
            Method::PATCH => client.patch(&url),
            Method::DELETE => client.delete(&url),
            Method::PUT => client.put(&url),
            _ => panic!("Unsupported method"),
        };

        if let Some(body) = body {
            req = req.json(&body);
        }

        req.send().await.unwrap()
    }

    async fn make_authenticated_request(
        server: &Server,
        method: Method,
        path: &str,
        body: Option<serde_json::Value>,
    ) -> reqwest::Response {
        let client = reqwest::Client::new();

        // First login to get session cookie
        let login_resp = client
            .post(&format!("{}/api/login", server.base_url))
            .json(&json!({
                "username": "slamb",
                "password": "hunter2"
            }))
            .send()
            .await
            .unwrap();

        assert_eq!(login_resp.status(), StatusCode::NO_CONTENT);

        // Extract session cookie from Set-Cookie header
        let cookie_header = login_resp
            .headers()
            .get("set-cookie")
            .and_then(|v| v.to_str().ok())
            .unwrap();

        // Get CSRF token from /api/ endpoint
        let csrf_token = if matches!(method, Method::POST | Method::PATCH | Method::DELETE) {
            let toplevel_resp = client
                .get(&format!("{}/api/", server.base_url))
                .header("Cookie", cookie_header)
                .send()
                .await
                .unwrap();

            let toplevel: serde_json::Value = toplevel_resp.json().await.unwrap();
            toplevel
                .get("user")
                .and_then(|u| u.get("session"))
                .and_then(|s| s.get("csrf"))
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
        } else {
            None
        };

        let url = format!("{}/api{}", server.base_url, path);
        let mut req = match method {
            Method::GET => client.get(&url),
            Method::POST => client.post(&url),
            Method::PATCH => client.patch(&url),
            Method::DELETE => client.delete(&url),
            Method::PUT => client.put(&url),
            _ => panic!("Unsupported method"),
        };

        // Add session cookie
        req = req.header("Cookie", cookie_header);

        if let Some(mut body) = body {
            // Add CSRF token to body for state-changing requests
            if let Some(csrf) = csrf_token {
                if let Some(obj) = body.as_object_mut() {
                    obj.insert("csrf".to_string(), json!(csrf));
                }
            }
            req = req.json(&body);
        } else if let Some(csrf) = csrf_token {
            // For requests with no body but needing CSRF
            req = req.json(&json!({"csrf": csrf}));
        }

        req.send().await.unwrap()
    }

    async fn create_test_server_with_permissions(perms: db::Permissions) -> Server {
        let server = Server::new(None).await;

        // Update the test user with the specified permissions
        let mut user_change = server.db.db.lock().users_by_id().get(&1).unwrap().change();
        user_change.permissions = perms;
        server.db.db.lock().apply_user_change(user_change).unwrap();

        server
    }

    #[tokio::test]
    async fn test_get_storage_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;

        let resp = make_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_storage_forbidden() {
        testutil::init();
        let server = create_test_server_with_permissions(db::Permissions::default()).await; // No view_video permission

        let resp = make_authenticated_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_storage_empty() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["storageDirs"].as_array().unwrap().len(), 1); // TestDb creates one dir
    }

    #[tokio::test]
    async fn test_get_storage_with_data() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        let dirs = json["storageDirs"].as_array().unwrap();
        assert!(!dirs.is_empty());

        // Check structure of first directory
        let dir = &dirs[0];
        assert!(dir["id"].is_number());
        assert!(dir["uuid"].is_string());
        assert!(dir["path"].is_string());
        assert!(dir["totalBytes"].is_number() || dir["totalBytes"].is_null());
        assert!(dir["usedBytes"].is_number());
        assert!(dir["streamsUsing"].is_array());
    }

    #[tokio::test]
    async fn test_post_storage_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;
        let tempdir = TempDir::new().unwrap();

        let body = json!({
            "path": tempdir.path().to_str().unwrap()
        });

        let resp = make_request(&server, Method::POST, "/storage", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_post_storage_forbidden() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true; // Has view_video but not admin_cameras
        let server = create_test_server_with_permissions(perms).await;
        let tempdir = TempDir::new().unwrap();

        let body = json!({
            "path": tempdir.path().to_str().unwrap()
        });

        let resp = make_authenticated_request(&server, Method::POST, "/storage", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_post_storage_success() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;
        let tempdir = TempDir::new().unwrap();

        let body = json!({
            "path": tempdir.path().to_str().unwrap()
        });

        let resp = make_authenticated_request(&server, Method::POST, "/storage", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        assert!(json["id"].is_number());
        assert!(json["uuid"].is_string());
    }

    #[tokio::test]
    async fn test_post_storage_invalid_path() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let body = json!({
            "path": "/nonexistent/path/that/should/not/exist"
        });

        let resp = make_authenticated_request(&server, Method::POST, "/storage", Some(body)).await;
        // The system returns 404 when the path doesn't exist rather than 400
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_storage_dir_success() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms).await;

        // Get the test storage directory ID
        let resp = make_authenticated_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let json: serde_json::Value = resp.json().await.unwrap();
        let dir_id = json["storageDirs"][0]["id"].as_i64().unwrap();

        let resp =
            make_authenticated_request(&server, Method::GET, &format!("/storage/{}", dir_id), None)
                .await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["id"].as_i64().unwrap(), dir_id);
        assert!(json["uuid"].is_string());
        assert!(json["path"].is_string());
    }

    #[tokio::test]
    async fn test_get_storage_dir_not_found() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::GET, "/storage/99999", None).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;

        let resp = make_request(&server, Method::GET, "/storage/1", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_patch_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;

        let body = json!({
            "csrf": "test-csrf-token"
        });

        let resp = make_request(&server, Method::PATCH, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_patch_storage_dir_forbidden() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true; // Has view_video but not admin_cameras
        let server = create_test_server_with_permissions(perms).await;

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::PATCH, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_patch_storage_dir_success() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::PATCH, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_delete_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;

        let body = json!({
            "csrf": "test-csrf-token"
        });

        let resp = make_request(&server, Method::DELETE, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_delete_storage_dir_forbidden() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true; // Has view_video but not admin_cameras
        let server = create_test_server_with_permissions(perms).await;

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::DELETE, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_delete_storage_dir_not_found() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::DELETE, "/storage/99999", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_unauthorized() {
        testutil::init();
        let server = Server::new(None).await;

        let resp = make_request(&server, Method::GET, "/storage-dirs", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_forbidden() {
        testutil::init();
        let server = create_test_server_with_permissions(db::Permissions::default()).await; // No view_video permission

        let resp = make_authenticated_request(&server, Method::GET, "/storage-dirs", None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_success() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::GET, "/storage-dirs", None).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        let dirs = json["dirs"].as_array().unwrap();
        assert!(!dirs.is_empty());

        // Check structure
        let dir = &dirs[0];
        assert!(dir["id"].is_number());
        assert!(dir["path"].is_string());
        // Should not have other fields like uuid, totalBytes, etc.
        assert!(!dir.as_object().unwrap().contains_key("uuid"));
        assert!(!dir.as_object().unwrap().contains_key("totalBytes"));
    }

    #[tokio::test]
    async fn test_invalid_json_payload() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp =
            make_authenticated_request(&server, Method::POST, "/storage", Some(json!("invalid")))
                .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn test_method_not_allowed() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::PUT, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_storage_dir_method_not_allowed() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.admin_cameras = true;
        let server = create_test_server_with_permissions(perms).await;

        let resp = make_authenticated_request(&server, Method::PUT, "/storage/1", None).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);

        let resp = make_authenticated_request(&server, Method::POST, "/storage/1", None).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}
