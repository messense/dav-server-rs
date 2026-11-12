//! Tests for GET on a collection with `DavConfig::collection_get_empty`.

#[cfg(feature = "memfs")]
mod collection_get_tests {
    use dav_server::{DavConfig, DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::{Request, StatusCode};

    /// Returns a server with `/dir/` and `/dir/a.txt`.
    async fn setup(config: impl FnOnce(DavConfig) -> DavConfig) -> DavHandler {
        let server = config(
            DavHandler::builder()
                .filesystem(MemFs::new())
                .locksystem(FakeLs::new()),
        )
        .build_handler();
        for (method, uri, body) in [("MKCOL", "/dir/", ""), ("PUT", "/dir/a.txt", "hello")] {
            let req = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::from(body))
                .unwrap();
            assert_eq!(server.handle(req).await.status(), StatusCode::CREATED);
        }
        server
    }

    async fn request(server: &DavHandler, method: &str, uri: &str) -> (StatusCode, String) {
        use futures_util::StreamExt;

        let req = Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::empty())
            .unwrap();
        let mut resp = server.handle(req).await;
        let status = resp.status();
        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        (status, String::from_utf8(data).unwrap())
    }

    #[tokio::test]
    async fn collection_get_is_unchanged_by_default() {
        let server = setup(|c| c).await;
        assert_eq!(
            request(&server, "GET", "/dir/").await.0,
            StatusCode::METHOD_NOT_ALLOWED
        );
        assert_eq!(request(&server, "GET", "/dir").await.0, StatusCode::FOUND);
    }

    #[tokio::test]
    async fn collection_get_empty_answers_an_empty_200() {
        let server = setup(|c| c.collection_get_empty(true)).await;
        for (method, uri) in [
            ("GET", "/dir/"),
            ("GET", "/dir"),
            ("GET", "/"),
            ("HEAD", "/dir/"),
        ] {
            assert_eq!(
                request(&server, method, uri).await,
                (StatusCode::OK, String::new()),
                "{method} {uri}"
            );
        }
        // Files are not affected.
        assert_eq!(
            request(&server, "GET", "/dir/a.txt").await,
            (StatusCode::OK, "hello".to_string())
        );
    }

    #[tokio::test]
    async fn collection_get_empty_takes_the_place_of_autoindex() {
        let server = setup(|c| c.autoindex(true)).await;
        let (status, body) = request(&server, "GET", "/dir/").await;
        assert_eq!(status, StatusCode::OK);
        assert!(body.contains("a.txt"), "no index in {body}");

        let server = setup(|c| c.autoindex(true).collection_get_empty(true)).await;
        assert_eq!(
            request(&server, "GET", "/dir/").await,
            (StatusCode::OK, String::new())
        );
    }
}
