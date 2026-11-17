//! Tests for the quota properties of collections (`get_quota_at`).

#[cfg(feature = "memfs")]
mod quota_tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use dav_server::{
        DavHandler,
        body::Body,
        davpath::DavPath,
        fakels::FakeLs,
        fs::{
            DavDirEntry, DavFile, DavFileSystem, DavMetaData, FsError, FsFuture, FsStream,
            OpenOptions, ReadDirMeta,
        },
        memfs::MemFs,
    };
    use http::{Request, StatusCode};

    /// A `MemFs` with a filesystem-wide quota of 5000 bytes used out of 100000. With
    /// `per_collection`, `/sub/` reports its own quota: 300 bytes used, 40000 available.
    #[derive(Clone)]
    struct QuotaFs {
        inner: MemFs,
        per_collection: bool,
        get_quota_calls: Arc<AtomicUsize>,
    }

    impl DavFileSystem for QuotaFs {
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

        fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.inner.create_dir(path)
        }

        fn get_quota(&'_ self) -> FsFuture<'_, (u64, Option<u64>)> {
            self.get_quota_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok((5000, Some(100_000))) })
        }

        fn get_quota_at<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, (u64, Option<u64>)> {
            let quota = if self.per_collection && path.as_url_string() == "/sub/" {
                Ok((300, Some(300 + 40_000)))
            } else {
                Err(FsError::NotImplemented)
            };
            Box::pin(async move { quota })
        }
    }

    /// Returns a server with `/sub/` and `/sub/a.txt`, and the `get_quota` call counter.
    async fn setup(per_collection: bool) -> (DavHandler, Arc<AtomicUsize>) {
        let get_quota_calls = Arc::new(AtomicUsize::new(0));
        let server = DavHandler::builder()
            .filesystem(Box::new(QuotaFs {
                inner: *MemFs::new(),
                per_collection,
                get_quota_calls: get_quota_calls.clone(),
            }))
            .locksystem(FakeLs::new())
            .build_handler();
        for (method, uri, body) in [("MKCOL", "/sub/", ""), ("PUT", "/sub/a.txt", "hello")] {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::from(body))
                .unwrap();
            assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        }
        (server, get_quota_calls)
    }

    async fn resp_to_string(mut resp: http::Response<Body>) -> String {
        use futures_util::StreamExt;

        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(data).unwrap()
    }

    /// PROPFIND `uri` with Depth 1 for both quota properties.
    async fn propfind_quota(server: &DavHandler, uri: &str) -> String {
        let req = Request::builder()
            .method("PROPFIND")
            .uri(uri)
            .header("Depth", "1")
            .body(Body::from(
                r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:">
  <D:prop>
    <D:quota-used-bytes/>
    <D:quota-available-bytes/>
  </D:prop>
</D:propfind>"#,
            ))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);
        resp_to_string(resp).await
    }

    /// The `<D:response>` for `href` in a multistatus body.
    fn response_for<'a>(body: &'a str, href: &str) -> &'a str {
        body.split("<D:response>")
            .find(|r| r.contains(&format!("<D:href>{href}</D:href>")))
            .unwrap_or_else(|| panic!("no response for {href} in {body}"))
    }

    fn assert_quota(body: &str, href: &str, used: u64, avail: u64) {
        let response = response_for(body, href);
        assert!(
            response.contains(&format!("<D:quota-used-bytes>{used}</D:quota-used-bytes>")),
            "{href}: expected quota-used-bytes {used} in {response}"
        );
        assert!(
            response.contains(&format!(
                "<D:quota-available-bytes>{avail}</D:quota-available-bytes>"
            )),
            "{href}: expected quota-available-bytes {avail} in {response}"
        );
    }

    #[tokio::test]
    async fn collections_report_the_filesystem_quota_by_default() {
        let (server, get_quota_calls) = setup(false).await;
        let body = propfind_quota(&server, "/").await;
        assert_quota(&body, "/", 5000, 95_000);
        assert_quota(&body, "/sub/", 5000, 95_000);
        // Looked up once for the whole request, as before.
        assert_eq!(get_quota_calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn collections_report_get_quota_at() {
        let (server, _) = setup(true).await;
        let body = propfind_quota(&server, "/").await;
        // `get_quota_at` is not implemented for `/`: the filesystem-wide quota.
        assert_quota(&body, "/", 5000, 95_000);
        assert_quota(&body, "/sub/", 300, 40_000);

        // Files keep reporting their own length as used space.
        let body = propfind_quota(&server, "/sub/").await;
        assert_quota(&body, "/sub/", 300, 40_000);
        assert_quota(&body, "/sub/a.txt", 5, 99_995);
    }
}
