//! Tests for the prefix of the DAV: namespace (`DavConfig::dav_prefix`).

#[cfg(feature = "memfs")]
mod dav_prefix_tests {
    use dav_server::{DavHandler, body::Body, memfs::MemFs, memls::MemLs};
    use http::{Request, StatusCode};
    use xmltree::{Element, XMLNode};

    /// Two handlers on the same file system and lock system: one with the default
    /// `D` prefix, one with `dav_prefix("d")`.
    fn handlers() -> (DavHandler, DavHandler) {
        let config = DavHandler::builder()
            .filesystem(MemFs::new())
            .locksystem(MemLs::new())
            .principal("/principals/users/alice");
        (
            config.clone().build_handler(),
            config.dav_prefix("d").build_handler(),
        )
    }

    async fn send(
        server: &DavHandler,
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (StatusCode, http::HeaderMap, String) {
        use futures_util::StreamExt;

        let mut req = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            req = req.header(*name, *value);
        }
        let mut resp = server
            .handle(req.body(Body::from(body.to_string())).unwrap())
            .await;
        let mut data = Vec::new();
        while let Some(chunk) = resp.body_mut().next().await {
            data.extend_from_slice(&chunk.unwrap());
        }
        (
            resp.status(),
            resp.headers().clone(),
            String::from_utf8(data).unwrap(),
        )
    }

    /// `/dir/a.txt` with a dead property, locked through `server`; returns the lock token.
    async fn setup(server: &DavHandler) -> String {
        for (method, uri, body) in [("MKCOL", "/dir/", ""), ("PUT", "/dir/a.txt", "hello")] {
            let (status, _, _) = send(server, method, uri, &[], body).await;
            assert_eq!(status, StatusCode::CREATED, "{method} {uri}");
        }
        #[cfg(feature = "caldav")]
        send(server, "MKCOL", "/calendars/", &[], "").await;
        #[cfg(feature = "carddav")]
        send(server, "MKCOL", "/addressbooks/", &[], "").await;

        let (status, headers, _) = send(
            server,
            "LOCK",
            "/dir/a.txt",
            &[("Timeout", "Second-600")],
            r#"<?xml version="1.0" encoding="utf-8"?>
<lockinfo xmlns="DAV:">
  <lockscope><exclusive/></lockscope>
  <locktype><write/></locktype>
  <owner><href>alice</href></owner>
</lockinfo>"#,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        headers["lock-token"]
            .to_str()
            .unwrap()
            .trim_matches(['<', '>'])
            .to_string()
    }

    /// The tree as a namespace-aware client sees it: namespace URIs instead of
    /// prefixes, and no namespace declarations.
    fn canonical(e: &Element) -> String {
        let mut s = format!("<{{{}}}{}", e.namespace.as_deref().unwrap_or(""), e.name);
        let mut attributes: Vec<_> = e.attributes.iter().collect();
        attributes.sort();
        for (name, value) in attributes {
            s += &format!(" {name}={value:?}");
        }
        s.push('>');
        for child in &e.children {
            match child {
                XMLNode::Element(c) => s += &canonical(c),
                XMLNode::Text(t) => s += t.trim(),
                _ => {}
            }
        }
        s + "</>"
    }

    /// Asserts that every element in the DAV: namespace uses `prefix`. A lock owner is
    /// skipped: it is stored with the lock, in the form of the handler that created it,
    /// and its content is returned as the client sent it.
    fn assert_dav_prefix(e: &Element, prefix: &str, body: &str) {
        if e.namespace.as_deref() == Some("DAV:") {
            if e.name == "owner" {
                return;
            }
            assert_eq!(e.prefix.as_deref(), Some(prefix), "<{}> in {body}", e.name);
        }
        for child in &e.children {
            if let XMLNode::Element(c) = child {
                assert_dav_prefix(c, prefix, body);
            }
        }
    }

    /// Sends the same request to both handlers and checks that the responses mean the
    /// same, and that the second one is written with `d:`.
    async fn compare(
        servers: &(DavHandler, DavHandler),
        method: &str,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> String {
        let (status_default, _, upper) = send(&servers.0, method, uri, headers, body).await;
        let (status, _, lower) = send(&servers.1, method, uri, headers, body).await;
        assert_eq!(status, status_default, "{method} {uri}");

        let upper_tree = Element::parse(upper.as_bytes())
            .unwrap_or_else(|e| panic!("{method} {uri}: {e} in {upper}"));
        let lower_tree = Element::parse(lower.as_bytes())
            .unwrap_or_else(|e| panic!("{method} {uri}: {e} in {lower}"));
        assert_eq!(
            canonical(&lower_tree),
            canonical(&upper_tree),
            "{method} {uri}"
        );

        assert_dav_prefix(&upper_tree, "D", &upper);
        assert_dav_prefix(&lower_tree, "d", &lower);
        // A stored dead property can still carry the client's own `xmlns:D`.
        assert!(!lower.contains("<D:"), "{method} {uri}: {lower}");
        lower
    }

    #[tokio::test]
    async fn dav_prefix_changes_only_the_prefix() {
        let servers = handlers();
        let token = setup(&servers.1).await;

        // allprop and propname, with a lock and a dead property.
        let (status, _, _) = send(
            &servers.1,
            "PROPPATCH",
            "/dir/a.txt",
            &[("If", &format!("(<{token}>)"))],
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:X="urn:x">
  <D:set><D:prop><X:color>blue</X:color></D:prop></D:set>
</D:propertyupdate>"#,
        )
        .await;
        assert_eq!(status, StatusCode::MULTI_STATUS);
        for body in [
            "",
            r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#,
            r#"<?xml version="1.0" encoding="utf-8"?><D:propfind xmlns:D="DAV:"><D:propname/></D:propfind>"#,
        ] {
            let lower = compare(&servers, "PROPFIND", "/dir/", &[("Depth", "1")], body).await;
            assert!(lower.contains("<d:multistatus"), "{lower}");
            assert!(lower.contains(r#"xmlns:d="DAV:""#), "{lower}");
            assert!(
                lower.contains("<d:response><d:href>/dir/</d:href>"),
                "{lower}"
            );
        }

        // Named properties, in a request that uses another prefix for DAV:, including
        // properties that are not found and are echoed back.
        let lower = compare(
            &servers,
            "PROPFIND",
            "/dir/a.txt",
            &[("Depth", "0")],
            r#"<?xml version="1.0" encoding="utf-8"?>
<P:propfind xmlns:P="DAV:" xmlns:X="urn:x">
  <P:prop>
    <P:resourcetype/><P:getetag/><P:lockdiscovery/><P:supportedlock/>
    <P:current-user-principal/><P:supported-privilege-set/>
    <P:quota-used-bytes/><P:no-such-property/><X:color/><X:missing/>
  </P:prop>
</P:propfind>"#,
        )
        .await;
        assert!(lower.contains("<d:lockdiscovery><d:activelock>"), "{lower}");
        assert!(
            lower.contains("<d:status>HTTP/1.1 404 Not Found</d:status>"),
            "{lower}"
        );

        let lower = compare(
            &servers,
            "PROPPATCH",
            "/dir/a.txt",
            &[("If", &format!("(<{token}>)"))],
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propertyupdate xmlns:D="DAV:" xmlns:X="urn:x">
  <D:set><D:prop><X:color>blue</X:color></D:prop></D:set>
</D:propertyupdate>"#,
        )
        .await;
        assert!(lower.contains("<d:multistatus"), "{lower}");

        // The refused Depth: infinity has a DAV:error body.
        let lower = compare(&servers, "PROPFIND", "/", &[("Depth", "infinity")], "").await;
        assert!(lower.contains(r#"<d:error xmlns:d="DAV:">"#), "{lower}");

        // A lock refresh answers with the lock discovery of the same lock.
        let lower = compare(
            &servers,
            "LOCK",
            "/dir/a.txt",
            &[("If", &format!("(<{token}>)")), ("Timeout", "Second-600")],
            "",
        )
        .await;
        assert!(lower.contains("<d:prop"), "{lower}");
    }

    #[cfg(feature = "caldav")]
    #[tokio::test]
    async fn dav_prefix_in_caldav_properties() {
        let servers = handlers();
        setup(&servers.1).await;
        let lower = compare(
            &servers,
            "PROPFIND",
            "/calendars/",
            &[("Depth", "0")],
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop><C:calendar-home-set/><D:supported-report-set/></D:prop>
</D:propfind>"#,
        )
        .await;
        assert!(lower.contains("<d:href>/calendars/</d:href>"), "{lower}");
        assert!(lower.contains("<d:supported-report>"), "{lower}");
    }

    #[cfg(feature = "carddav")]
    #[tokio::test]
    async fn dav_prefix_in_carddav_properties() {
        let servers = handlers();
        setup(&servers.1).await;
        let lower = compare(
            &servers,
            "PROPFIND",
            "/addressbooks/",
            &[("Depth", "0")],
            r#"<?xml version="1.0" encoding="utf-8"?>
<D:propfind xmlns:D="DAV:" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <D:prop><CARD:addressbook-home-set/></D:prop>
</D:propfind>"#,
        )
        .await;
        assert!(lower.contains("<d:href>/addressbooks/</d:href>"), "{lower}");
    }

    #[test]
    #[should_panic(expected = "used by dav-server for another namespace")]
    fn dav_prefix_refuses_a_prefix_in_use() {
        let _: dav_server::DavConfig = DavHandler::builder().dav_prefix("oc");
    }
}
