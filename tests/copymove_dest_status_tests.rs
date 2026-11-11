//! Tests for the status of a COPY or MOVE whose destination fails.

#[cfg(feature = "memfs")]
mod copymove_dest_status_tests {
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

    /// A `MemFs` that refuses to create `/refused/`, to remove `/keep.txt`, and to copy
    /// to `/new/bad.txt`.
    #[derive(Clone)]
    struct RefuseFs {
        inner: MemFs,
    }

    fn refuse<'a>() -> FsFuture<'a, ()> {
        Box::pin(async { Err(FsError::Forbidden) })
    }

    impl DavFileSystem for RefuseFs {
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
            if path.as_url_string() == "/refused/" {
                return refuse();
            }
            self.inner.create_dir(path)
        }

        fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            if path.as_url_string() == "/keep.txt" {
                return refuse();
            }
            self.inner.remove_file(path)
        }

        fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            if to.as_url_string() == "/new/bad.txt" {
                return refuse();
            }
            self.inner.copy(from, to)
        }
    }

    /// A server with `/a.txt`, `/keep.txt`, `/dir/bad.txt` and `/dir/good.txt`.
    async fn setup() -> DavHandler {
        let server = DavHandler::builder()
            .filesystem(Box::new(RefuseFs {
                inner: *MemFs::new(),
            }))
            .locksystem(FakeLs::new())
            .build_handler();
        for (method, uri) in [
            ("PUT", "/a.txt"),
            ("PUT", "/keep.txt"),
            ("MKCOL", "/dir/"),
            ("PUT", "/dir/bad.txt"),
            ("PUT", "/dir/good.txt"),
        ] {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::from(if method == "PUT" { "data" } else { "" }))
                .unwrap();
            assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        }
        server
    }

    async fn request(server: &DavHandler, method: &str, uri: &str, dest: &str) -> StatusCode {
        let req = Request::builder()
            .method(method)
            .uri(uri)
            .header("Destination", dest)
            .header("Overwrite", "T")
            .body(Body::empty())
            .unwrap();
        server.handle(req).await.status()
    }

    #[tokio::test]
    async fn a_refused_destination_answers_a_plain_status() {
        let server = setup().await;
        // The destination collection cannot be created.
        assert_eq!(
            request(&server, "COPY", "/dir/", "/refused/").await,
            StatusCode::FORBIDDEN
        );
        // The existing destination cannot be removed.
        assert_eq!(
            request(&server, "COPY", "/a.txt", "/keep.txt").await,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            request(&server, "MOVE", "/a.txt", "/keep.txt").await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_refused_member_still_answers_207() {
        let server = setup().await;
        assert_eq!(
            request(&server, "COPY", "/dir/", "/new/").await,
            StatusCode::MULTI_STATUS
        );
    }
}
