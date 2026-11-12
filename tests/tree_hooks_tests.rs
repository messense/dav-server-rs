//! Tests for the optional `remove_tree` and `copy_tree` file system methods.

#[cfg(feature = "memfs")]
mod tree_hooks_tests {
    use std::sync::{Arc, Mutex};

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

    /// A `MemFs` that logs the calls that change the tree. With `tree_ops`, it
    /// implements `remove_tree` and `copy_tree` by only logging them, and fails them
    /// for `/fail/`.
    #[derive(Clone)]
    struct TreeFs {
        inner: MemFs,
        tree_ops: bool,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl TreeFs {
        fn log(&self, entry: String) {
            self.log.lock().unwrap().push(entry);
        }

        fn tree_op(&self, entry: String, path: &DavPath) -> FsFuture<'_, ()> {
            let result = if !self.tree_ops {
                Err(FsError::NotImplemented)
            } else if path.as_url_string() == "/fail/" {
                Err(FsError::Forbidden)
            } else {
                self.log(entry);
                Ok(())
            };
            Box::pin(async move { result })
        }
    }

    impl DavFileSystem for TreeFs {
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
            self.log(format!("create_dir {}", path.as_url_string()));
            self.inner.create_dir(path)
        }

        fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.log(format!("remove_dir {}", path.as_url_string()));
            self.inner.remove_dir(path)
        }

        fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.log(format!("remove_file {}", path.as_url_string()));
            self.inner.remove_file(path)
        }

        fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            self.log(format!(
                "copy {} {}",
                from.as_url_string(),
                to.as_url_string()
            ));
            self.inner.copy(from, to)
        }

        fn remove_tree<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.tree_op(format!("remove_tree {}", path.as_url_string()), path)
        }

        fn copy_tree<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            let entry = format!("copy_tree {} {}", from.as_url_string(), to.as_url_string());
            self.tree_op(entry, from)
        }
    }

    /// A server with `/dir/a.txt`, `/dir/sub/b.txt` and `/fail/c.txt`, and an empty log.
    async fn setup(tree_ops: bool) -> (DavHandler, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let server = DavHandler::builder()
            .filesystem(Box::new(TreeFs {
                inner: *MemFs::new(),
                tree_ops,
                log: log.clone(),
            }))
            .locksystem(FakeLs::new())
            .build_handler();
        for (method, uri) in [
            ("MKCOL", "/dir/"),
            ("PUT", "/dir/a.txt"),
            ("MKCOL", "/dir/sub/"),
            ("PUT", "/dir/sub/b.txt"),
            ("MKCOL", "/fail/"),
            ("PUT", "/fail/c.txt"),
        ] {
            let status = request(&server, method, uri, &[]).await;
            assert_eq!(status, StatusCode::CREATED, "{method} {uri}");
        }
        log.lock().unwrap().clear();
        (server, log)
    }

    async fn request(
        server: &DavHandler,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
    ) -> StatusCode {
        let mut req = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let body = if method == "PUT" { "data" } else { "" };
        server
            .handle(req.body(Body::from(body)).unwrap())
            .await
            .status()
    }

    fn take(log: &Mutex<Vec<String>>) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[tokio::test]
    async fn without_tree_operations_collections_are_walked() {
        let (server, log) = setup(false).await;

        let copy = [("Destination", "/copy/")];
        assert_eq!(
            request(&server, "COPY", "/dir/", &copy).await,
            StatusCode::CREATED
        );
        let mut calls = take(&log);
        calls.sort();
        assert_eq!(
            calls,
            [
                "copy /dir/a.txt /copy/a.txt",
                "copy /dir/sub/b.txt /copy/sub/b.txt",
                "create_dir /copy/",
                "create_dir /copy/sub/",
            ]
        );

        assert_eq!(
            request(&server, "DELETE", "/dir/", &[]).await,
            StatusCode::NO_CONTENT
        );
        let mut calls = take(&log);
        calls.sort();
        assert_eq!(
            calls,
            [
                "remove_dir /dir/",
                "remove_dir /dir/sub/",
                "remove_file /dir/a.txt",
                "remove_file /dir/sub/b.txt",
            ]
        );
    }

    #[tokio::test]
    async fn collections_use_tree_operations() {
        let (server, log) = setup(true).await;

        let copy = [("Destination", "/copy/")];
        assert_eq!(
            request(&server, "COPY", "/dir/", &copy).await,
            StatusCode::CREATED
        );
        assert_eq!(take(&log), ["copy_tree /dir/ /copy/"]);

        assert_eq!(
            request(&server, "DELETE", "/dir/", &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["remove_tree /dir/"]);

        // Files are not affected.
        assert_eq!(
            request(&server, "DELETE", "/dir/a.txt", &[]).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["remove_file /dir/a.txt"]);
    }

    #[tokio::test]
    async fn copy_overwrite_removes_the_destination_tree_first() {
        let (server, log) = setup(true).await;
        assert_eq!(
            request(&server, "MKCOL", "/copy/", &[]).await,
            StatusCode::CREATED
        );
        take(&log);

        let copy = [("Destination", "/copy/"), ("Overwrite", "T")];
        assert_eq!(
            request(&server, "COPY", "/dir/", &copy).await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["remove_tree /copy/", "copy_tree /dir/ /copy/"]);
    }

    #[tokio::test]
    async fn copy_tree_is_not_used_for_depth_0_or_into_the_source() {
        let (server, log) = setup(true).await;

        let copy = [("Destination", "/copy/"), ("Depth", "0")];
        assert_eq!(
            request(&server, "COPY", "/dir/", &copy).await,
            StatusCode::CREATED
        );
        assert_eq!(take(&log), ["create_dir /copy/"]);

        // Into its own subtree: the source is walked, its other members can still be
        // copied in one step.
        let copy = [("Destination", "/dir/inner/")];
        assert_eq!(
            request(&server, "COPY", "/dir/", &copy).await,
            StatusCode::CREATED
        );
        let mut calls = take(&log);
        calls.sort();
        assert_eq!(
            calls,
            [
                "copy /dir/a.txt /dir/inner/a.txt",
                "copy_tree /dir/sub/ /dir/inner/sub/",
                "create_dir /dir/inner/",
            ]
        );
    }

    #[tokio::test]
    async fn tree_operation_errors_are_reported() {
        let (server, log) = setup(true).await;
        assert_eq!(
            request(&server, "DELETE", "/fail/", &[]).await,
            StatusCode::FORBIDDEN
        );
        let copy = [("Destination", "/copy/")];
        assert_eq!(
            request(&server, "COPY", "/fail/", &copy).await,
            StatusCode::FORBIDDEN
        );
        assert!(take(&log).is_empty());
    }
}
