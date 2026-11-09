#![cfg(feature = "memfs")]

use dav_server::{DavHandler, body::Body, memfs::MemFs, memls::MemLs};
use http::{Request, StatusCode};
#[cfg(feature = "proppatch")]
use http_body_util::BodyExt;

async fn reject_deep_xml(method: &str, root: &str) {
    let server = DavHandler::builder()
        .filesystem(MemFs::new())
        .locksystem(MemLs::new())
        .build_handler();

    for closed in [false, true] {
        let mut xml = format!(r#"<D:{root} xmlns:D="DAV:">"#);
        xml.push_str(&"<a>".repeat(3_000));
        if closed {
            xml.push_str(&"</a>".repeat(3_000));
            xml.push_str(&format!("</D:{root}>"));
        }
        assert!(xml.len() < 65_536);
        let request = Request::builder()
            .method(method)
            .uri("/")
            .header("Depth", "0")
            .header("Content-Type", "application/xml")
            .body(Body::from(xml))
            .unwrap();
        let response = server.handle(request).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    // The worker must remain usable after rejecting the hostile requests.
    let request = Request::builder()
        .method("PROPFIND")
        .uri("/")
        .header("Depth", "0")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        server.handle(request).await.status(),
        StatusCode::MULTI_STATUS
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn propfind_rejects_deep_xml() {
    tokio::spawn(reject_deep_xml("PROPFIND", "propfind"))
        .await
        .unwrap();
}

#[cfg(feature = "proppatch")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn proppatch_rejects_deep_xml() {
    tokio::spawn(reject_deep_xml("PROPPATCH", "propertyupdate"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn lock_rejects_deep_xml() {
    tokio::spawn(reject_deep_xml("LOCK", "lockinfo"))
        .await
        .unwrap();
}

#[cfg(feature = "proppatch")]
async fn check_stored_property_depth(allprop: bool) {
    use dav_server::{davpath::DavPath, fs::DavFileSystem, fs::DavProp};

    for depth in [64, 65, 3_001] {
        let fs = MemFs::new();
        // Bypass request parsing, as a custom backend or existing stored data can.
        let value = "<a>".repeat(depth - 1) + "stored-value" + &"</a>".repeat(depth - 1);
        let property = DavProp::new("deep".into(), "T".into(), "urn:depth-test".into(), value);
        fs.patch_props(&DavPath::new("/").unwrap(), vec![(true, property)])
            .await
            .unwrap();
        let server = DavHandler::builder().filesystem(fs).build_handler();
        let body = if allprop {
            r#"<D:propfind xmlns:D="DAV:"><D:allprop/></D:propfind>"#
        } else {
            r#"<D:propfind xmlns:D="DAV:"><D:prop><T:deep xmlns:T="urn:depth-test"/></D:prop></D:propfind>"#
        };
        let request = Request::builder()
            .method("PROPFIND")
            .uri("/")
            .header("Depth", "0")
            .body(Body::from(body))
            .unwrap();
        let response = server.handle(request).await;
        assert_eq!(response.status(), StatusCode::MULTI_STATUS);
        // Property XML is parsed lazily while producing the response body.
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let xml = String::from_utf8(bytes.to_vec()).unwrap();
        assert_eq!(xml.contains("stored-value"), depth == 64);
        assert!(!xml.contains("<a>".repeat(64).as_str()));
    }
}

#[cfg(feature = "proppatch")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn propfind_allprop_limits_stored_xml_depth() {
    tokio::spawn(check_stored_property_depth(true))
        .await
        .unwrap();
}

#[cfg(feature = "proppatch")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn propfind_named_property_limits_stored_xml_depth() {
    tokio::spawn(check_stored_property_depth(false))
        .await
        .unwrap();
}
