#[cfg(all(target_os = "linux", feature = "localfs"))]
mod dav_tests {
    use dav_server::{DavHandler, DavOptionHide, body::Body, fakels::FakeLs, localfs::LocalFs};
    use http::{Request, StatusCode};

    fn setup_dav_server_symlink() -> DavHandler {
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST");
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST/normal_dir");
        let _ = std::fs::create_dir("/tmp/DAV_SERVER_TEST/.hidden_folder");
        let _ = std::fs::write("/tmp/DAV_SERVER_TEST/normal_dir/normal_file", "hello world");
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
    async fn test_dav_propfind_namespace_consistent() {
        let server = setup_dav_server_symlink();

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/normal_dir")
            .body(Body::from(r#"<?xml version="1.0"?><a:propfind xmlns:a="DAV:"><a:prop><a:resourcetype/><a:getcontentlength/><a:getlastmodified/></a:prop></a:propfind>"#))
            .unwrap();

        let resp = server.handle(req).await;
        let content = resp_to_string(resp).await;
        assert!(content.contains(r#"<D:resourcetype></D:resourcetype>"#));
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

#[cfg(all(feature = "memfs", feature = "proppatch"))]
mod proppatch_atomic_tests {
    use std::time::SystemTime;

    use dav_server::{
        DavHandler,
        body::Body,
        davpath::DavPath,
        fakels::FakeLs,
        fs::{
            DavDirEntry, DavFile, DavFileSystem, DavMetaData, DavProp, FsError, FsFuture, FsStream,
            OpenOptions, ReadDirMeta,
        },
        memfs::MemFs,
    };
    use futures_util::future::BoxFuture;
    use http::{Request, StatusCode};

    const MTIME_HTTP: &str = "Tue, 07 Feb 2023 17:06:21 GMT";

    /// A `MemFs` whose `set_modified` and `patch_props` can be made to fail.
    #[derive(Clone)]
    struct ScriptedFs {
        inner: MemFs,
        set_modified_error: Option<FsError>,
        refuse_dead_props: bool,
    }

    impl DavFileSystem for ScriptedFs {
        fn open<'a>(
            &'a self,
            path: &'a DavPath,
            options: OpenOptions,
        ) -> FsFuture<'a, Box<dyn DavFile>> {
            self.inner.open(path, options)
        }

        fn read_dir<'a>(
            &'a self,
            path: &'a DavPath,
            meta: ReadDirMeta,
        ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
            self.inner.read_dir(path, meta)
        }

        fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
            self.inner.metadata(path)
        }

        fn symlink_metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
            self.inner.symlink_metadata(path)
        }

        fn set_modified<'a>(&'a self, path: &'a DavPath, tm: SystemTime) -> FsFuture<'a, ()> {
            match self.set_modified_error {
                Some(err) => Box::pin(async move { Err(err) }),
                None => self.inner.set_modified(path, tm),
            }
        }

        fn have_props<'a>(&'a self, path: &'a DavPath) -> BoxFuture<'a, bool> {
            self.inner.have_props(path)
        }

        fn patch_props<'a>(
            &'a self,
            path: &'a DavPath,
            patch: Vec<(bool, DavProp)>,
        ) -> FsFuture<'a, Vec<(StatusCode, DavProp)>> {
            if self.refuse_dead_props {
                let refused = patch
                    .into_iter()
                    .map(|(_, prop)| (StatusCode::FORBIDDEN, prop))
                    .collect();
                return Box::pin(async move { Ok(refused) });
            }
            self.inner.patch_props(path, patch)
        }

        fn get_props<'a>(
            &'a self,
            path: &'a DavPath,
            do_content: bool,
        ) -> FsFuture<'a, Vec<DavProp>> {
            self.inner.get_props(path, do_content)
        }

        fn get_prop<'a>(&'a self, path: &'a DavPath, prop: DavProp) -> FsFuture<'a, Vec<u8>> {
            self.inner.get_prop(path, prop)
        }
    }

    /// Returns a server whose file system holds `/notes.txt`.
    async fn setup(set_modified_error: Option<FsError>, refuse_dead_props: bool) -> DavHandler {
        let server = DavHandler::builder()
            .filesystem(Box::new(ScriptedFs {
                inner: *MemFs::new(),
                set_modified_error,
                refuse_dead_props,
            }))
            .locksystem(FakeLs::new())
            .build_handler();
        let req = Request::builder()
            .method("PUT")
            .uri("/notes.txt")
            .body(Body::from("hello"))
            .unwrap();
        assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        server
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(data).unwrap()
    }

    async fn request(server: &DavHandler, method: &str, body: String) -> String {
        let req = Request::builder()
            .method(method)
            .uri("/notes.txt")
            .header("Depth", "0")
            .body(Body::from(body))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        resp_to_string(resp).await
    }

    /// PROPPATCH that sets `Win32LastModifiedTime` and `extra`, a property element in the
    /// `D:prop` of the request.
    async fn proppatch(server: &DavHandler, extra: &str) -> String {
        request(
            server,
            "PROPPATCH",
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:Z="urn:schemas-microsoft-com:" xmlns:X="urn:x">
  <D:set>
    <D:prop>
      <Z:Win32LastModifiedTime>{MTIME_HTTP}</Z:Win32LastModifiedTime>
      {extra}
    </D:prop>
  </D:set>
</D:propertyupdate>"#
            ),
        )
        .await
    }

    async fn propfind(server: &DavHandler, prop: &str) -> String {
        request(
            server,
            "PROPFIND",
            format!(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:X="urn:x"><D:prop>{prop}</D:prop></D:propfind>"#
            ),
        )
        .await
    }

    /// The status of the propstat in `body` that contains `name`.
    fn status_of<'a>(body: &'a str, name: &str) -> &'a str {
        let propstat = body
            .split("<D:propstat>")
            .skip(1)
            .find(|p| p.contains(name))
            .unwrap_or_else(|| panic!("no propstat with {name} in {body}"));
        let status = &propstat[propstat.find("<D:status>HTTP/1.1 ").unwrap() + 19..];
        &status[..status.find("</D:status>").unwrap()]
    }

    #[tokio::test]
    async fn refused_property_leaves_the_modification_time_unchanged() {
        let server = setup(None, false).await;
        let before = propfind(&server, "<D:getlastmodified/>").await;

        let body = proppatch(&server, r#"<D:getetag>"x"</D:getetag>"#).await;
        assert_eq!(status_of(&body, "getetag"), "403 Forbidden", "{body}");
        assert_eq!(
            status_of(&body, "Win32LastModifiedTime"),
            "424 Failed Dependency",
            "{body}"
        );

        assert_eq!(propfind(&server, "<D:getlastmodified/>").await, before);
        assert!(!before.contains(MTIME_HTTP), "{before}");
    }

    #[tokio::test]
    async fn set_modified_error_is_reported_and_stops_the_dead_properties() {
        let server = setup(Some(FsError::Forbidden), false).await;

        let body = proppatch(&server, "<X:one>1</X:one>").await;
        assert_eq!(
            status_of(&body, "Win32LastModifiedTime"),
            "403 Forbidden",
            "{body}"
        );
        assert_eq!(status_of(&body, "X:one"), "424 Failed Dependency", "{body}");

        let body = propfind(&server, "<X:one/>").await;
        assert_eq!(status_of(&body, "X:one"), "404 Not Found", "{body}");
    }

    #[tokio::test]
    async fn refused_dead_property_restores_the_modification_time() {
        let server = setup(None, true).await;
        let before = propfind(&server, "<D:getlastmodified/>").await;

        let body = proppatch(&server, "<X:one>1</X:one>").await;
        assert_eq!(status_of(&body, "X:one"), "403 Forbidden", "{body}");
        assert_eq!(
            status_of(&body, "Win32LastModifiedTime"),
            "424 Failed Dependency",
            "{body}"
        );

        assert_eq!(propfind(&server, "<D:getlastmodified/>").await, before);
    }

    #[tokio::test]
    async fn not_implemented_set_modified_is_still_reported_as_ok() {
        let server = setup(Some(FsError::NotImplemented), false).await;

        let body = proppatch(&server, "<X:one>1</X:one>").await;
        assert_eq!(
            status_of(&body, "Win32LastModifiedTime"),
            "200 OK",
            "{body}"
        );
        assert_eq!(status_of(&body, "X:one"), "200 OK", "{body}");
    }

    #[tokio::test]
    async fn successful_proppatch_sets_both() {
        let server = setup(None, false).await;

        let body = proppatch(&server, "<X:one>1</X:one>").await;
        assert_eq!(
            status_of(&body, "Win32LastModifiedTime"),
            "200 OK",
            "{body}"
        );

        assert!(
            propfind(&server, "<D:getlastmodified/>")
                .await
                .contains(MTIME_HTTP)
        );
        assert!(propfind(&server, "<X:one/>").await.contains(">1</X:one>"));
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

#[cfg(feature = "memfs")]
mod getetag_tests {
    use dav_server::{DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::{Request, StatusCode};

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

    async fn put(server: &DavHandler, uri: &str, if_match: Option<&str>) -> StatusCode {
        let mut req = Request::builder().method("PUT").uri(uri);
        if let Some(tag) = if_match {
            req = req.header("If-Match", tag);
        }
        let req = req.body(Body::from("hello")).unwrap();
        server.handle(req).await.status()
    }

    async fn propfind_getetag(server: &DavHandler, uri: &str) -> String {
        let req = Request::builder()
            .method("PROPFIND")
            .uri(uri)
            .header("Depth", "0")
            .body(Body::from(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:getetag/>
  </D:prop>
</D:propfind>"#,
            ))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        let body = resp_to_string(resp).await;
        let start = body.find("<D:getetag>").expect("getetag missing") + "<D:getetag>".len();
        let end = body[start..].find("</D:getetag>").unwrap() + start;
        body[start..end].to_string()
    }

    #[tokio::test]
    async fn propfind_getetag_is_quoted_and_round_trips_as_if_match() {
        let server = setup();
        assert_eq!(put(&server, "/file.txt", None).await, StatusCode::CREATED);

        let getetag = propfind_getetag(&server, "/file.txt").await;
        assert!(
            getetag.len() > 2 && getetag.starts_with('"') && getetag.ends_with('"'),
            "getetag is not a quoted entity-tag: {getetag}"
        );

        let req = Request::builder()
            .method("HEAD")
            .uri("/file.txt")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let header_etag = resp.headers().get("etag").unwrap().to_str().unwrap();
        assert_eq!(getetag, header_etag);

        assert_eq!(
            put(&server, "/file.txt", Some(&getetag)).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(
            put(&server, "/file.txt", Some("\"stale\"")).await,
            StatusCode::PRECONDITION_FAILED
        );
    }
}

#[cfg(feature = "memfs")]
mod propfind_prop_tests {
    use dav_server::{
        DavHandler,
        body::Body,
        davpath::DavPath,
        fakels::FakeLs,
        fs::{
            DavDirEntry, DavFile, DavFileSystem, DavMetaData, DavProp, FsFuture, FsStream,
            OpenOptions, ReadDirMeta,
        },
        memfs::MemFs,
    };
    use futures_util::future::BoxFuture;
    use http::{Request, StatusCode};

    /// A `MemFs` that serves the property `X:extra` only through `get_props`, like a
    /// backend that computes its properties in one place. `get_prop` keeps its default,
    /// which returns `NotImplemented`.
    #[derive(Clone)]
    struct GetPropsOnlyFs {
        inner: MemFs,
    }

    impl DavFileSystem for GetPropsOnlyFs {
        fn open<'a>(
            &'a self,
            path: &'a DavPath,
            options: OpenOptions,
        ) -> FsFuture<'a, Box<dyn DavFile>> {
            self.inner.open(path, options)
        }

        fn read_dir<'a>(
            &'a self,
            path: &'a DavPath,
            meta: ReadDirMeta,
        ) -> FsFuture<'a, FsStream<Box<dyn DavDirEntry>>> {
            self.inner.read_dir(path, meta)
        }

        fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
            self.inner.metadata(path)
        }

        fn symlink_metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, Box<dyn DavMetaData>> {
            self.inner.symlink_metadata(path)
        }

        fn have_props<'a>(&'a self, _path: &'a DavPath) -> BoxFuture<'a, bool> {
            Box::pin(async { true })
        }

        fn get_props<'a>(
            &'a self,
            _path: &'a DavPath,
            _do_content: bool,
        ) -> FsFuture<'a, Vec<DavProp>> {
            Box::pin(async {
                Ok(vec![DavProp::new(
                    "extra".to_string(),
                    "X".to_string(),
                    "http://example.com/ns".to_string(),
                    "value".to_string(),
                )])
            })
        }
    }

    /// Returns a server whose `fs` holds the file `/file.txt`.
    async fn setup(fs: Box<dyn dav_server::fs::GuardedFileSystem<()>>) -> DavHandler {
        let server = DavHandler::builder()
            .filesystem(fs)
            .locksystem(FakeLs::new())
            .build_handler();
        let req = Request::builder()
            .method("PUT")
            .uri("/file.txt")
            .body(Body::from("hello"))
            .unwrap();
        assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        server
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

    async fn propfind(server: &DavHandler, body: &'static str) -> String {
        let req = Request::builder()
            .method("PROPFIND")
            .uri("/file.txt")
            .header("Depth", "0")
            .body(Body::from(body))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        resp_to_string(resp).await
    }

    /// Returns the `<D:propstat>` element of `body` with the given status.
    fn propstat<'a>(body: &'a str, status: &str) -> Option<&'a str> {
        body.split("<D:propstat>")
            .skip(1)
            .find(|p| p.contains(&format!("<D:status>HTTP/1.1 {status}</D:status>")))
    }

    #[tokio::test]
    async fn propfind_prop_reports_missing_property_as_404() {
        let server = setup(MemFs::new()).await;

        let body = propfind(
            &server,
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:X="http://example.com/ns">
  <D:prop>
    <D:getcontentlength/>
    <X:missing/>
  </D:prop>
</D:propfind>"#,
        )
        .await;

        let ok = propstat(&body, "200 OK").expect("no 200 propstat");
        assert!(ok.contains("getcontentlength"), "{body}");
        assert!(!ok.contains("missing"), "{body}");

        let not_found = propstat(&body, "404 Not Found").expect("no 404 propstat");
        assert!(not_found.contains("missing"), "{body}");
        assert!(!not_found.contains("getcontentlength"), "{body}");
    }

    #[tokio::test]
    async fn propfind_prop_reports_property_from_get_props_as_200_only() {
        let server = setup(Box::new(GetPropsOnlyFs {
            inner: *MemFs::new(),
        }))
        .await;

        let body = propfind(
            &server,
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:X="http://example.com/ns">
  <D:prop>
    <X:extra/>
    <X:missing/>
  </D:prop>
</D:propfind>"#,
        )
        .await;

        let ok = propstat(&body, "200 OK").expect("no 200 propstat");
        assert!(ok.contains("extra"), "{body}");
        assert!(!ok.contains("missing"), "{body}");

        let not_found = propstat(&body, "404 Not Found").expect("no 404 propstat");
        assert!(not_found.contains("missing"), "{body}");
        assert!(!not_found.contains("extra"), "{body}");
    }

    #[tokio::test]
    async fn propfind_allprop_and_propname_omit_missing_properties() {
        let server = setup(MemFs::new()).await;

        for request in [
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#,
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#,
        ] {
            let body = propfind(&server, request).await;
            assert!(propstat(&body, "200 OK").is_some(), "{body}");
            assert!(propstat(&body, "404 Not Found").is_none(), "{body}");
        }
    }
}
