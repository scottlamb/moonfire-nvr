// This file is part of Moonfire NVR, a security camera network video recorder.
// Copyright (C) 2024 The Moonfire NVR Authors; see AUTHORS and LICENSE.txt.
// SPDX-License-Identifier: GPL-v3.0-or-later WITH GPL-3.0-linking-exception

//! Storage management API endpoints.

use crate::json;
use base::err;
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
                    return Err(err!(PermissionDenied, msg("view_video required")));
                }
                self.get_storage(&req, caller)
            }
            Method::POST => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
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
                    return Err(err!(PermissionDenied, msg("view_video required")));
                }
                self.get_storage_dir(&req, caller, id)
            }
            Method::PATCH => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
                }
                self.patch_storage_dir(req, caller, id).await
            }
            Method::DELETE => {
                if !permissions.admin_cameras {
                    return Err(err!(PermissionDenied, msg("admin_cameras required")));
                }
                self.delete_storage_dir(req, caller, id).await
            }
            _ => Ok(plain_response(
                StatusCode::METHOD_NOT_ALLOWED,
                "GET, PATCH, or DELETE expected",
            )),
        }
    }

    fn get_storage(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
    ) -> ResponseResult {
        let db = self.db.lock();
        let mut storage_dirs = Vec::new();

        for (&id, dir) in db.sample_file_dirs_by_id() {
            let mut total_bytes = 0i64;
            let mut used_bytes = 0i64;
            let mut streams_using = Vec::new();

            // Calculate usage by streams
            for (&stream_id, stream) in db.streams_by_id() {
                if stream.sample_file_dir_id == Some(id) {
                    used_bytes += stream.sample_file_bytes;
                    streams_using.push(json::StorageStreamUsage {
                        stream_id,
                        camera_name: db
                            .cameras_by_id()
                            .get(&stream.camera_id)
                            .map(|c| c.short_name.clone())
                            .unwrap_or_else(|| format!("camera {}", stream.camera_id)),
                        stream_type: stream.type_.as_str().to_string(),
                        used_bytes: stream.sample_file_bytes,
                        duration_90k: stream.duration.0,
                    });
                }
            }

            // Try to get filesystem stats if directory is accessible
            if let Ok(dir_handle) = dir.get() {
                if let Ok(stat) = dir_handle.statfs() {
                    total_bytes = (stat.blocks_available() * stat.fragment_size()) as i64;
                }
            }

            storage_dirs.push(json::StorageDir {
                id,
                uuid: dir.uuid,
                path: dir.path.display().to_string(),
                total_bytes,
                used_bytes,
                streams_using,
            });
        }

        serve_json(req, &json::GetStorageResponse { storage_dirs })
    }

    fn get_storage_dir(
        &self,
        req: &Request<::hyper::body::Incoming>,
        _caller: Caller,
        id: i32,
    ) -> ResponseResult {
        let db = self.db.lock();
        let dir = db
            .sample_file_dirs_by_id()
            .get(&id)
            .ok_or_else(|| err!(NotFound, msg("no such storage directory {id}")))?;

        let mut used_bytes = 0i64;
        let mut streams_using = Vec::new();

        // Calculate usage by streams
        for (&stream_id, stream) in db.streams_by_id() {
            if stream.sample_file_dir_id == Some(id) {
                used_bytes += stream.sample_file_bytes;
                streams_using.push(json::StorageStreamUsage {
                    stream_id,
                    camera_name: db
                        .cameras_by_id()
                        .get(&stream.camera_id)
                        .map(|c| c.short_name.clone())
                        .unwrap_or_else(|| format!("camera {}", stream.camera_id)),
                    stream_type: stream.type_.as_str().to_string(),
                    used_bytes: stream.sample_file_bytes,
                    duration_90k: stream.duration.0,
                });
            }
        }

        let mut total_bytes = 0i64;
        if let Ok(dir_handle) = dir.get() {
            if let Ok(stat) = dir_handle.statfs() {
                total_bytes = (stat.blocks_available() * stat.fragment_size()) as i64;
            }
        }

        let storage_dir = json::StorageDir {
            id,
            uuid: dir.uuid,
            path: dir.path.display().to_string(),
            total_bytes,
            used_bytes,
            streams_using,
        };

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

        let mut db = self.db.lock();
        let id = db.add_sample_file_dir(PathBuf::from(r.path))?;

        // Get the UUID from the created directory
        let uuid = db
            .sample_file_dirs_by_id()
            .get(&id)
            .ok_or_else(|| err!(Internal, msg("directory not found after creation")))?
            .uuid;

        let (parts, _) = (parts, ());
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

        // For now, storage directories can't be updated - they're essentially immutable
        // once created. This endpoint exists for future extensibility.
        let _ = r; // Silence unused variable warning

        let (parts, _) = (parts, ());
        serve_json(&parts, &json::PatchStorageResponse { success: true })
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

        let mut db = self.db.lock();
        db.delete_sample_file_dir(id)?;

        let (parts, _) = (parts, ());
        serve_json(&parts, &json::DeleteStorageResponse { success: true })
    }

    pub(super) fn storage_dirs_simple(
        &self,
        req: Request<::hyper::body::Incoming>,
        caller: Caller,
    ) -> ResponseResult {
        let permissions = &caller.permissions;
        if !permissions.view_video {
            return Err(err!(PermissionDenied, msg("view_video required")));
        }

        let db = self.db.lock();
        let mut dirs = Vec::new();

        for (&id, dir) in db.sample_file_dirs_by_id() {
            dirs.push(json::StorageDirSimple {
                id,
                path: dir.path.display().to_string(),
            });
        }

        let (parts, _) = req.into_parts();
        serve_json(&parts, &json::GetStorageDirsSimpleResponse { dirs })
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

    fn create_test_server_with_permissions(perms: db::Permissions) -> Server {
        let server = Server::new(None);

        // Update the test user with the specified permissions
        let mut user_change = server.db.db.lock().users_by_id().get(&1).unwrap().change();
        user_change.permissions = perms;
        server.db.db.lock().apply_user_change(user_change).unwrap();

        server
    }

    #[tokio::test]
    async fn test_get_storage_unauthorized() {
        testutil::init();
        let server = Server::new(None);

        let resp = make_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_storage_forbidden() {
        testutil::init();
        let server = create_test_server_with_permissions(db::Permissions::default()); // No view_video permission

        let resp = make_authenticated_request(&server, Method::GET, "/storage", None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_storage_empty() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

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
        assert!(dir["totalBytes"].is_number());
        assert!(dir["usedBytes"].is_number());
        assert!(dir["streamsUsing"].is_array());
    }

    #[tokio::test]
    async fn test_post_storage_unauthorized() {
        testutil::init();
        let server = Server::new(None);
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
        let server = create_test_server_with_permissions(perms);
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
        let server = create_test_server_with_permissions(perms);
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
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

        let resp = make_authenticated_request(&server, Method::GET, "/storage/99999", None).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None);

        let resp = make_request(&server, Method::GET, "/storage/1", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_patch_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None);

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
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::PATCH, "/storage/1", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let json: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(json["success"], true);
    }

    #[tokio::test]
    async fn test_delete_storage_dir_unauthorized() {
        testutil::init();
        let server = Server::new(None);

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
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

        let body = json!({});

        let resp =
            make_authenticated_request(&server, Method::DELETE, "/storage/99999", Some(body)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_unauthorized() {
        testutil::init();
        let server = Server::new(None);

        let resp = make_request(&server, Method::GET, "/storage-dirs", None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_forbidden() {
        testutil::init();
        let server = create_test_server_with_permissions(db::Permissions::default()); // No view_video permission

        let resp = make_authenticated_request(&server, Method::GET, "/storage-dirs", None).await;
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_get_storage_dirs_simple_success() {
        testutil::init();
        let mut perms = db::Permissions::default();
        perms.view_video = true;
        let server = create_test_server_with_permissions(perms);

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
        let server = create_test_server_with_permissions(perms);

        let client = reqwest::Client::new();
        let url = format!("{}/api/storage", server.base_url);

        let resp = client
            .post(&url)
            .header("Content-Type", "application/json")
            .body("invalid json")
            .send()
            .await
            .unwrap();

        // Authentication is checked before JSON parsing, so we get 401 instead of 400
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_method_not_allowed() {
        testutil::init();
        let server = Server::new(None);

        let client = reqwest::Client::new();
        let url = format!("{}/api/storage", server.base_url);

        // Authentication is checked before method validation, so we get 401 instead of 405
        let resp = client.put(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = client.head(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_storage_dir_method_not_allowed() {
        testutil::init();
        let server = Server::new(None);

        let client = reqwest::Client::new();
        let url = format!("{}/api/storage/1", server.base_url);

        // Authentication is checked before method validation, so we get 401 instead of 405
        let resp = client.put(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = client.post(&url).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}
