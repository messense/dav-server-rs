//! Tests for WebDAV principal support (RFC5397 current-user-principal,
//! RFC3744 principal resources and privilege sets).
//!
//! The test filesystem wraps `MemFs` and serves one virtual principal
//! resource at `/principals/users/testuser` via the optional
//! `principal_metadata()` method.

#[cfg(feature = "memfs")]
mod principal_tests {
    use std::time::SystemTime;

    use dav_server::{
        DavHandler,
        body::Body,
        davpath::DavPath,
        fakels::FakeLs,
        fs::{
            DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsResult,
            FsStream, OpenOptions, ReadDirMeta,
        },
        memfs::MemFs,
    };
    use http::{Request, StatusCode};

    const PRINCIPAL: &str = "/principals/users/testuser";
    const FAILING: &str = "/failing";

    /// Metadata for the virtual principal resource.
    #[derive(Debug, Clone)]
    struct PrincipalMeta;

    impl DavMetaData for PrincipalMeta {
        fn len(&self) -> u64 {
            0
        }
        fn modified(&self) -> FsResult<SystemTime> {
            Ok(SystemTime::UNIX_EPOCH)
        }
        fn is_dir(&self) -> bool {
            true
        }
        fn is_symlink(&self) -> bool {
            false
        }
        #[cfg(feature = "caldav")]
        fn is_calendar(&self, _path: &DavPath) -> bool {
            false
        }
        #[cfg(feature = "carddav")]
        fn is_addressbook(&self, _path: &DavPath) -> bool {
            false
        }
    }

    fn is_principal(path: &DavPath) -> bool {
        path.with_prefix().as_url_string().trim_end_matches('/') == PRINCIPAL
    }

    fn is_failing(path: &DavPath) -> bool {
        path.with_prefix().as_url_string().trim_end_matches('/') == FAILING
    }

    #[derive(Clone)]
    struct PrincipalFs {
        inner: MemFs,
    }

    impl DavFileSystem for PrincipalFs {
        fn open<'a>(
            &'a self,
            path: &'a DavPath,
            options: OpenOptions,
        ) -> FsFuture<'a, Box<dyn DavFile>> {
            if is_principal(path) {
                // principal resources are neither retrievable nor writable
                return Box::pin(async { Err(FsError::Forbidden) });
            }
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

        fn principal_metadata<'a>(
            &'a self,
            path: &'a DavPath,
        ) -> FsFuture<'a, Option<Box<dyn DavMetaData>>> {
            if is_failing(path) {
                // Simulate a backend that cannot decide: the lookup must not
                // be treated as "no principal here".
                return Box::pin(async { Err(FsError::Forbidden) });
            }
            if is_principal(path) {
                return Box::pin(async {
                    Ok(Some(Box::new(PrincipalMeta) as Box<dyn DavMetaData>))
                });
            }
            Box::pin(async { Ok(None) })
        }

        fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            // Delegated unconditionally so the tests can seed the principal
            // collection; open()/remove_* still reject principal paths.
            self.inner.create_dir(path)
        }

        fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            if is_principal(path) {
                return Box::pin(async { Err(FsError::Forbidden) });
            }
            self.inner.remove_dir(path)
        }

        fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            if is_principal(path) {
                return Box::pin(async { Err(FsError::Forbidden) });
            }
            self.inner.remove_file(path)
        }
    }

    fn setup() -> DavHandler {
        DavHandler::builder()
            .filesystem(Box::new(PrincipalFs {
                inner: *MemFs::new(),
            }) as _)
            .locksystem(FakeLs::new())
            .principal(PRINCIPAL)
            .build_handler()
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            match chunk {
                Ok(bytes) => data.extend_from_slice(&bytes),
                Err(e) => panic!("Error reading body stream: {e}"),
            }
        }
        String::from_utf8(data).unwrap_or_default()
    }

    async fn mkcol(server: &DavHandler, path: &str) -> StatusCode {
        let req = Request::builder()
            .method("MKCOL")
            .uri(path)
            .body(Body::empty())
            .unwrap();
        server.handle(req).await.status()
    }

    async fn propfind(server: &DavHandler, path: &str, depth: &str, body: &str) -> String {
        let req = Request::builder()
            .method("PROPFIND")
            .uri(path)
            .header("Depth", depth)
            .body(Body::from(body))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        resp_to_string(resp).await
    }

    #[tokio::test]
    async fn test_principal_resourcetype() {
        let server = setup();
        let content = propfind(
            &server,
            &format!("{PRINCIPAL}/"),
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains("<D:collection"));
        assert!(content.contains("<D:principal"));
    }

    #[tokio::test]
    async fn test_principal_url_and_owner() {
        let server = setup();
        let content = propfind(
            &server,
            &format!("{PRINCIPAL}/"),
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:principal-URL/><D:owner/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains("<D:principal-URL>"));
        assert!(content.contains("<D:owner>"));
        assert!(content.contains(&format!(">{PRINCIPAL}/<")));
    }

    #[tokio::test]
    async fn test_owner_not_found_on_regular_resource() {
        let server = setup();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:owner/></D:prop></D:propfind>"#,
        )
        .await;
        // Props the resource does not have are left out of the response.
        assert!(!content.contains("<D:owner>"));
    }

    #[tokio::test]
    async fn test_current_user_principal() {
        let server = setup();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains(&format!(">{PRINCIPAL}<")));

        // without a configured principal the property does not exist (404)
        let server = DavHandler::builder()
            .filesystem(Box::new(PrincipalFs {
                inner: *MemFs::new(),
            }) as _)
            .locksystem(FakeLs::new())
            .build_handler();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:current-user-principal/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(!content.contains("<D:current-user-principal"));
    }

    #[tokio::test]
    async fn test_privilege_sets() {
        let server = setup();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:supported-privilege-set/><D:current-user-privilege-set/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains("<D:supported-privilege-set>"));
        assert!(content.contains("<D:current-user-privilege-set>"));
        assert!(content.contains("<D:all"));
    }

    #[tokio::test]
    async fn test_options_on_principal() {
        let server = setup();
        let req = Request::builder()
            .method("OPTIONS")
            .uri(format!("{PRINCIPAL}/"))
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let allow = resp.headers().get("allow").unwrap().to_str().unwrap();
        assert!(allow.contains("PROPFIND"));
    }

    #[tokio::test]
    async fn test_propname_includes_principal_properties() {
        let server = setup();
        let content = propfind(
            &server,
            &format!("{PRINCIPAL}/"),
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#,
        )
        .await;

        for name in [
            "principal-URL",
            "owner",
            "supported-privilege-set",
            "current-user-privilege-set",
        ] {
            assert!(
                content.contains(&format!("<D:{name}")),
                "missing {name} in {content}"
            );
        }

        // propname responses contain names without values: the only href is
        // the response href and no privilege tree is emitted.
        assert_eq!(content.matches("<D:href").count(), 1);
        assert!(!content.contains("<D:privilege"));
    }

    #[tokio::test]
    async fn test_propname_principal_properties_respect_availability() {
        let server = setup();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#,
        )
        .await;

        // These only exist on principal resources.
        assert!(!content.contains("<D:principal-URL"));
        assert!(!content.contains("<D:owner"));

        // Privilege properties exist on every resource (and a principal is
        // configured here, so the current-user variant exists too).
        assert!(content.contains("<D:supported-privilege-set"));
        assert!(content.contains("<D:current-user-privilege-set"));
    }

    #[tokio::test]
    async fn test_principal_writes_rejected() {
        let server = setup();

        // Seed the principal collection (the test backend allows creating it).
        assert_eq!(mkcol(&server, "/principals").await, StatusCode::CREATED);
        assert_eq!(
            mkcol(&server, "/principals/users").await,
            StatusCode::CREATED
        );
        assert_eq!(
            mkcol(&server, &format!("{PRINCIPAL}/")).await,
            StatusCode::CREATED
        );

        // DELETE is refused by the filesystem backend.
        let req = Request::builder()
            .method("DELETE")
            .uri(format!("{PRINCIPAL}/"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(server.handle(req).await.status(), StatusCode::FORBIDDEN);

        // GET is not served for principal resources.
        let req = Request::builder()
            .method("GET")
            .uri(format!("{PRINCIPAL}/"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            server.handle(req).await.status(),
            StatusCode::METHOD_NOT_ALLOWED
        );

        // MKCOL on an existing collection is refused.
        assert_eq!(
            mkcol(&server, &format!("{PRINCIPAL}/")).await,
            StatusCode::METHOD_NOT_ALLOWED
        );
    }

    #[tokio::test]
    async fn test_principal_propfind_depth1() {
        let server = setup();
        assert_eq!(mkcol(&server, "/principals").await, StatusCode::CREATED);
        assert_eq!(
            mkcol(&server, "/principals/users").await,
            StatusCode::CREATED
        );
        assert_eq!(
            mkcol(&server, &format!("{PRINCIPAL}/")).await,
            StatusCode::CREATED
        );

        let content = propfind(
            &server,
            "/principals/users/",
            "1",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains(&format!(">{PRINCIPAL}/<")));
        assert!(content.contains("<D:principal"));
    }

    #[tokio::test]
    async fn test_principal_lookup_error_is_propagated() {
        let server = setup();
        assert_eq!(
            mkcol(&server, &format!("{FAILING}/")).await,
            StatusCode::CREATED
        );

        // PROPFIND on a resource whose principal lookup failed must not fall
        // back to ordinary metadata (which would yield 207 Multi-Status).
        let req = Request::builder()
            .method("PROPFIND")
            .uri(format!("{FAILING}/"))
            .header("Depth", "0")
            .body(Body::from(
                r#"<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/></D:prop></D:propfind>"#,
            ))
            .unwrap();
        assert_eq!(server.handle(req).await.status(), StatusCode::FORBIDDEN);

        // Same for a direct GET/HEAD metadata lookup.
        let req = Request::builder()
            .method("GET")
            .uri(format!("{FAILING}/"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(server.handle(req).await.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_propfind_skips_entry_with_principal_lookup_error() {
        let server = setup();
        assert_eq!(
            mkcol(&server, &format!("{FAILING}/")).await,
            StatusCode::CREATED
        );
        assert_eq!(mkcol(&server, "/visible/").await, StatusCode::CREATED);

        let content = propfind(
            &server,
            "/",
            "1",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><D:resourcetype/></D:prop></D:propfind>"#,
        )
        .await;

        // The failed entry is skipped instead of being emitted with ordinary
        // metadata (and possibly recursed into); other entries remain.
        assert!(!content.contains(&format!(">{FAILING}/<")));
        assert!(content.contains(">/visible/<"));
    }

    #[cfg(feature = "caldav")]
    #[tokio::test]
    async fn test_calendar_home_set_on_principal() {
        let server = setup();
        let content = propfind(
            &server,
            &format!("{PRINCIPAL}/"),
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains("<C:calendar-home-set>"));
        assert!(content.contains(">/calendars/</D:href>"));
    }

    #[cfg(feature = "caldav")]
    #[tokio::test]
    async fn test_calendar_home_set_requires_principal_flag() {
        // The configured principal URL matches the requested path, but the
        // backend does not flag it as a principal resource (default Ok(None)).
        let server = DavHandler::builder()
            .filesystem(Box::new(PrincipalFs {
                inner: *MemFs::new(),
            }) as _)
            .locksystem(FakeLs::new())
            .principal("/")
            .build_handler();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><C:calendar-home-set xmlns:C="urn:ietf:params:xml:ns:caldav"/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(!content.contains("<C:calendar-home-set"));
    }

    #[cfg(feature = "carddav")]
    #[tokio::test]
    async fn test_addressbook_home_set_on_principal() {
        let server = setup();
        let content = propfind(
            &server,
            &format!("{PRINCIPAL}/"),
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><CARD:addressbook-home-set xmlns:CARD="urn:ietf:params:xml:ns:carddav"/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(content.contains("<CARD:addressbook-home-set>"));
        assert!(content.contains(">/addressbooks/</D:href>"));
    }

    #[cfg(feature = "carddav")]
    #[tokio::test]
    async fn test_addressbook_home_set_requires_principal_flag() {
        // Matching principal URL, but not flagged as a principal resource.
        let server = DavHandler::builder()
            .filesystem(Box::new(PrincipalFs {
                inner: *MemFs::new(),
            }) as _)
            .locksystem(FakeLs::new())
            .principal("/")
            .build_handler();
        let content = propfind(
            &server,
            "/",
            "0",
            r#"<D:propfind xmlns:D="DAV:"><D:prop><CARD:addressbook-home-set xmlns:CARD="urn:ietf:params:xml:ns:carddav"/></D:prop></D:propfind>"#,
        )
        .await;
        assert!(!content.contains("<CARD:addressbook-home-set"));
    }
}
