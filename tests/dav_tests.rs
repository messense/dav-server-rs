#[cfg(all(target_os = "linux", feature = "localfs"))]
mod dav_tests {
    use dav_server::{DavHandler, DavOptionHide, body::Body, fakels::FakeLs, localfs::LocalFs};
    use http::{Request, StatusCode};

    fn setup_dav_server_symlink() -> DavHandler {
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST");
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST/normal_dir");
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST/.hidden_folder");
        let _ = std::os::unix::fs::symlink(
            "/tmp/DAV_SERVER_TEST/normal_dir",
            "/tmp/DAV_SERVER_TEST/symlink_to_dir",
        );

        DavHandler::builder()
            // We need LocalFs to test for symlinks
            .filesystem(LocalFs::new("/tmp/DAV_SERVER_TEST", true, false, false))
            .locksystem(FakeLs::new())
            .autoindex(true)
            .hide_symlinks(true)
            .hide_dot_prefix(DavOptionHide::Always)
            .build_handler()
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        let body = resp.body_mut();

        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(e) => panic!("Error reading body stream: {}", e),
            }
        }

        String::from_utf8(data).unwrap_or_else(|_| "".to_string())
    }

    #[tokio::test]
    async fn test_dav_symlink_propfind_one() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/symlink_to_dir")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dav_symlink_propfind_dir() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let resp_text = resp_to_string(resp).await;
        assert!(!resp_text.contains("/symlink_to_dir"));
    }

    #[tokio::test]
    async fn test_dav_symlink_get_autoindex_one() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("GET")
            .uri("/symlink_to_dir")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dav_symlink_get_autoindex_dir() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_text = resp_to_string(resp).await;
        assert!(!resp_text.contains("/symlink_to_dir"));
    }

    #[tokio::test]
    async fn test_dav_dotprefix_propfind_one() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/.hidden_folder")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dav_dotprefix_propfind_dir() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let resp_text = resp_to_string(resp).await;
        assert!(!resp_text.contains("/.hidden_folder"));
    }

    #[tokio::test]
    async fn test_dav_dotprefix_get_autoindex_one() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("GET")
            .uri("/.hidden_folder")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_dav_dotprefix_get_autoindex_dir() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("GET")
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let resp_text = resp_to_string(resp).await;
        assert!(!resp_text.contains("/.hidden_folder"));
    }
}

#[cfg(all(unix, feature = "localfs"))]
mod localfs_umask_tests {
    use dav_server::{DavHandler, body::Body, localfs::LocalFs};
    use http::{Request, StatusCode};
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    static UMASK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    struct UmaskGuard(libc::mode_t);

    impl UmaskGuard {
        fn set(mask: libc::mode_t) -> Self {
            let old = unsafe { libc::umask(mask) };
            Self(old)
        }
    }

    impl Drop for UmaskGuard {
        fn drop(&mut self) {
            unsafe {
                libc::umask(self.0);
            }
        }
    }

    fn unix_mode(path: &std::path::Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn tempdir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "dav-umask-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn put_and_mkcol_honor_umask() {
        let _lock = UMASK_LOCK.lock().await;
        let _umask = UmaskGuard::set(0o002);
        let dir = tempdir();

        let server = DavHandler::builder()
            .filesystem(LocalFs::new(&dir, true, false, false))
            .build_handler();

        let put = server
            .handle(
                Request::builder()
                    .method("PUT")
                    .uri("/group.txt")
                    .body(Body::from("hi"))
                    .unwrap(),
            )
            .await;
        assert!(put.status().is_success(), "{}", put.status());
        assert_eq!(
            unix_mode(&dir.join("group.txt")),
            0o664,
            "PUT file should be 0664 under umask 002, not 0644"
        );

        let mk = server
            .handle(
                Request::builder()
                    .method("MKCOL")
                    .uri("/album")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
        assert_eq!(mk.status(), StatusCode::CREATED, "{}", mk.status());
        assert_eq!(
            unix_mode(&dir.join("album")),
            0o775,
            "MKCOL dir should be 0775 under umask 002, not 0755"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
#[cfg(feature = "memfs")]
mod oc_timestamp_tests {
    use dav_server::{DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::{Request, StatusCode};

    const MTIME: &str = "1675789581";
    const CTIME: &str = "1577836800";
    const MTIME_HTTP: &str = "Tue, 07 Feb 2023 17:06:21 GMT";
    const CTIME_RFC3339: &str = "2020-01-01T00:00:00Z";

    fn setup() -> DavHandler {
        DavHandler::builder()
            .filesystem(MemFs::new())
            .locksystem(FakeLs::new())
            .build_handler()
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        let body = resp.body_mut();

        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(e) => panic!("Error reading body stream: {}", e),
            }
        }

        String::from_utf8(data).unwrap_or_else(|_| "".to_string())
    }

    async fn propfind_body(server: &DavHandler, uri: &str) -> String {
        let req = Request::builder()
            .method("PROPFIND")
            .uri(uri)
            .header("Depth", "0")
            .body(Body::from(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:creationdate/>
    <D:getlastmodified/>
  </D:prop>
</D:propfind>"#,
            ))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        resp_to_string(resp).await
    }

    #[tokio::test]
    async fn put_oc_mtime_and_ctime() {
        let server = setup();

        let req = Request::builder()
            .method("PUT")
            .uri("/notes.txt")
            .header("X-OC-MTime", MTIME)
            .header("X-OC-CTime", CTIME)
            .body(Body::from("hello"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers()
                .get("x-oc-mtime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );
        assert_eq!(
            resp.headers()
                .get("x-oc-ctime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );
        assert_eq!(
            resp.headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok()),
            Some(MTIME_HTTP)
        );

        let body = propfind_body(&server, "/notes.txt").await;
        assert!(
            body.contains(MTIME_HTTP),
            "getlastmodified missing in {body}"
        );
        assert!(
            body.contains(CTIME_RFC3339),
            "creationdate missing in {body}"
        );
    }

    #[tokio::test]
    async fn put_without_oc_headers_has_no_accepted() {
        let server = setup();

        let req = Request::builder()
            .method("PUT")
            .uri("/plain.txt")
            .body(Body::from("hello"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert!(resp.headers().get("x-oc-mtime").is_none());
        assert!(resp.headers().get("x-oc-ctime").is_none());
    }

    #[tokio::test]
    async fn put_malformed_oc_mtime_is_bad_request() {
        let server = setup();

        let req = Request::builder()
            .method("PUT")
            .uri("/bad.txt")
            .header("X-OC-MTime", "not-a-timestamp")
            .body(Body::from("hello"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        let req = Request::builder()
            .method("GET")
            .uri("/bad.txt")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn put_overwrite_updates_mtime() {
        let server = setup();

        let req = Request::builder()
            .method("PUT")
            .uri("/notes.txt")
            .body(Body::from("v1"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        let req = Request::builder()
            .method("PUT")
            .uri("/notes.txt")
            .header("X-OC-MTime", MTIME)
            .body(Body::from("v2"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            resp.headers()
                .get("x-oc-mtime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );
        assert_eq!(
            resp.headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok()),
            Some(MTIME_HTTP)
        );
    }

    #[tokio::test]
    async fn mkcol_oc_mtime_and_ctime() {
        let server = setup();

        let req = Request::builder()
            .method("MKCOL")
            .uri("/photos")
            .header("X-OC-MTime", MTIME)
            .header("X-OC-CTime", CTIME)
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers()
                .get("x-oc-mtime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );
        assert_eq!(
            resp.headers()
                .get("x-oc-ctime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );

        let body = propfind_body(&server, "/photos").await;
        assert!(
            body.contains(MTIME_HTTP),
            "getlastmodified missing in {body}"
        );
        assert!(
            body.contains(CTIME_RFC3339),
            "creationdate missing in {body}"
        );
    }

    #[tokio::test]
    async fn mkcol_malformed_oc_mtime_is_bad_request() {
        let server = setup();

        let req = Request::builder()
            .method("MKCOL")
            .uri("/bad-dir")
            .header("X-OC-MTime", "-1")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }
}

#[cfg(feature = "localfs")]
mod oc_timestamp_localfs_tests {
    use dav_server::{DavHandler, body::Body, localfs::LocalFs};
    use http::{Request, StatusCode};
    use std::time::{Duration, UNIX_EPOCH};

    const MTIME: u64 = 1675789581;
    const MTIME_HTTP: &str = "Tue, 07 Feb 2023 17:06:21 GMT";

    #[tokio::test]
    async fn put_oc_mtime_localfs() {
        let dir = std::env::temp_dir().join(format!(
            "dav-oc-mtime-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let server = DavHandler::builder()
            .filesystem(LocalFs::new(&dir, true, false, false))
            .build_handler();

        let req = Request::builder()
            .method("PUT")
            .uri("/notes.txt")
            .header("X-OC-MTime", MTIME.to_string())
            .body(Body::from("hello"))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        assert_eq!(
            resp.headers()
                .get("x-oc-mtime")
                .and_then(|v| v.to_str().ok()),
            Some("accepted")
        );
        assert_eq!(
            resp.headers()
                .get("last-modified")
                .and_then(|v| v.to_str().ok()),
            Some(MTIME_HTTP)
        );

        let meta = std::fs::metadata(dir.join("notes.txt")).unwrap();
        let modified = meta.modified().unwrap();
        let expected = UNIX_EPOCH + Duration::from_secs(MTIME);
        let delta = modified
            .duration_since(expected)
            .or_else(|_| expected.duration_since(modified))
            .unwrap();
        assert!(
            delta < Duration::from_secs(1),
            "localfs mtime {modified:?} not close to {expected:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(all(feature = "memfs", feature = "proppatch"))]
mod win32_mtime_tests {
    use dav_server::{DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::{Request, StatusCode};

    const MTIME_HTTP: &str = "Tue, 07 Feb 2023 17:06:21 GMT";

    fn setup() -> DavHandler {
        DavHandler::builder()
            .filesystem(MemFs::new())
            .locksystem(FakeLs::new())
            .build_handler()
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        let body = resp.body_mut();

        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(e) => panic!("Error reading body stream: {}", e),
            }
        }

        String::from_utf8(data).unwrap_or_else(|_| "".to_string())
    }

    fn proppatch(uri: &str, mtime: &str) -> Request<Body> {
        Request::builder()
            .method("PROPPATCH")
            .uri(uri)
            .body(Body::from(format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:">
  <D:set>
    <D:prop>
      <Z:Win32LastModifiedTime>{mtime}</Z:Win32LastModifiedTime>
    </D:prop>
  </D:set>
</D:propertyupdate>"#
            )))
            .unwrap()
    }

    #[tokio::test]
    async fn proppatch_win32_last_modified_sets_mtime() {
        let server = setup();

        let resp = server
            .handle(
                Request::builder()
                    .method("PUT")
                    .uri("/notes.txt")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = server.handle(proppatch("/notes.txt", MTIME_HTTP)).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let body = resp_to_string(resp).await;
        assert!(
            body.contains("200 OK") || body.contains("HTTP/1.1 200"),
            "expected 200 for Win32LastModifiedTime, got {body}"
        );

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/notes.txt")
            .header("Depth", "0")
            .body(Body::from(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getlastmodified/>
  </D:prop>
</D:propfind>"#,
            ))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let body = resp_to_string(resp).await;
        assert!(
            body.contains(MTIME_HTTP),
            "getlastmodified missing {MTIME_HTTP} in {body}"
        );
    }

    #[tokio::test]
    async fn proppatch_win32_last_modified_malformed_is_conflict() {
        let server = setup();

        let resp = server
            .handle(
                Request::builder()
                    .method("PUT")
                    .uri("/notes.txt")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = server.handle(proppatch("/notes.txt", "not-a-date")).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let body = resp_to_string(resp).await;
        assert!(
            body.contains("409") || body.contains("Conflict"),
            "expected 409 for malformed Win32LastModifiedTime, got {body}"
        );
    }
}

#[cfg(all(feature = "localfs", feature = "proppatch"))]
mod win32_mtime_localfs_tests {
    use dav_server::{DavHandler, body::Body, localfs::LocalFs};
    use http::{Request, StatusCode};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    const MTIME: u64 = 1675789581;
    const MTIME_HTTP: &str = "Tue, 07 Feb 2023 17:06:21 GMT";

    #[tokio::test]
    async fn proppatch_win32_last_modified_localfs() {
        let dir = std::env::temp_dir().join(format!(
            "dav-win32-mtime-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();

        let server = DavHandler::builder()
            .filesystem(LocalFs::new(&dir, true, false, false))
            .build_handler();

        let resp = server
            .handle(
                Request::builder()
                    .method("PUT")
                    .uri("/notes.txt")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        let resp = server
            .handle(
                Request::builder()
                    .method("PROPPATCH")
                    .uri("/notes.txt")
                    .body(Body::from(format!(
                        r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:">
  <D:set>
    <D:prop>
      <Z:Win32LastModifiedTime>{MTIME_HTTP}</Z:Win32LastModifiedTime>
    </D:prop>
  </D:set>
</D:propertyupdate>"#
                    )))
                    .unwrap(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let meta = std::fs::metadata(dir.join("notes.txt")).unwrap();
        let modified = meta.modified().unwrap();
        let expected = UNIX_EPOCH + Duration::from_secs(MTIME);
        let delta = modified
            .duration_since(expected)
            .or_else(|_| expected.duration_since(modified))
            .unwrap();
        assert!(
            delta < Duration::from_secs(1),
            "localfs mtime {modified:?} not close to {expected:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
