//! Tests for the optional `rename_replace` file system method.

#[cfg(feature = "memfs")]
mod rename_replace_tests {
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

    /// A `MemFs` that logs renames and removals. With `replace`, it implements
    /// `rename_replace` with `MemFs::rename` (which replaces a file), and fails it for
    /// `/fail.txt`.
    #[derive(Clone)]
    struct ReplaceFs {
        inner: MemFs,
        replace: bool,
        log: Arc<Mutex<Vec<String>>>,
    }

    impl ReplaceFs {
        fn log(&self, entry: String) {
            self.log.lock().unwrap().push(entry);
        }
    }

    impl DavFileSystem for ReplaceFs {
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

        fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<'a, ()> {
            self.log(format!("remove_file {}", path.as_url_string()));
            self.inner.remove_file(path)
        }

        fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            self.inner.copy(from, to)
        }

        fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            self.log(format!(
                "rename {} {}",
                from.as_url_string(),
                to.as_url_string()
            ));
            self.inner.rename(from, to)
        }

        fn rename_replace<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<'a, ()> {
            if !self.replace {
                return Box::pin(async { Err(FsError::NotImplemented) });
            }
            if from.as_url_string() == "/fail.txt" {
                return Box::pin(async { Err(FsError::Forbidden) });
            }
            self.log(format!(
                "rename_replace {} {}",
                from.as_url_string(),
                to.as_url_string()
            ));
            self.inner.rename(from, to)
        }
    }

    /// A server with `/a.txt`, `/b.txt` and `/fail.txt`, and an empty log.
    async fn setup(replace: bool) -> (DavHandler, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let server = DavHandler::builder()
            .filesystem(Box::new(ReplaceFs {
                inner: *MemFs::new(),
                replace,
                log: log.clone(),
            }))
            .locksystem(FakeLs::new())
            .build_handler();
        for name in ["a", "b", "fail"] {
            let req = Request::builder()
                .method("PUT")
                .uri(format!("/{name}.txt"))
                .body(Body::from(name))
                .unwrap();
            assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        }
        (server, log)
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

    async fn get(server: &DavHandler, uri: &str) -> String {
        use futures_util::StreamExt;

        let req = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let mut resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK, "GET {uri}");
        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        String::from_utf8(data).unwrap()
    }

    fn take(log: &Mutex<Vec<String>>) -> Vec<String> {
        std::mem::take(&mut *log.lock().unwrap())
    }

    #[tokio::test]
    async fn move_overwrite_removes_the_destination_by_default() {
        let (server, log) = setup(false).await;
        assert_eq!(
            request(&server, "MOVE", "/a.txt", "/b.txt").await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["remove_file /b.txt", "rename /a.txt /b.txt"]);
        assert_eq!(get(&server, "/b.txt").await, "a");
    }

    #[tokio::test]
    async fn move_overwrite_uses_rename_replace() {
        let (server, log) = setup(true).await;
        assert_eq!(
            request(&server, "MOVE", "/a.txt", "/b.txt").await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["rename_replace /a.txt /b.txt"]);
        assert_eq!(get(&server, "/b.txt").await, "a");

        // Without an existing destination, it is a plain rename.
        assert_eq!(
            request(&server, "MOVE", "/b.txt", "/c.txt").await,
            StatusCode::CREATED
        );
        assert_eq!(take(&log), ["rename /b.txt /c.txt"]);
    }

    #[tokio::test]
    async fn failed_rename_replace_keeps_the_destination() {
        let (server, log) = setup(true).await;
        assert_eq!(
            request(&server, "MOVE", "/fail.txt", "/b.txt").await,
            StatusCode::FORBIDDEN
        );
        assert!(take(&log).is_empty());
        assert_eq!(get(&server, "/b.txt").await, "b");
        assert_eq!(get(&server, "/fail.txt").await, "fail");
    }

    #[tokio::test]
    async fn copy_does_not_use_rename_replace() {
        let (server, log) = setup(true).await;
        assert_eq!(
            request(&server, "COPY", "/a.txt", "/b.txt").await,
            StatusCode::NO_CONTENT
        );
        assert_eq!(take(&log), ["remove_file /b.txt"]);
    }
}
