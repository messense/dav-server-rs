#[cfg(feature = "carddav")]
mod carddav_tests {
    use dav_server::{DavHandler, body::Body, carddav::*, fakels::FakeLs, memfs::MemFs};
    use http::{Method, Request, StatusCode};

    fn setup_carddav_server() -> DavHandler {
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

    #[tokio::test]
    async fn test_carddav_options() {
        let server = setup_carddav_server();

        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let dav_header = resp.headers().get("DAV").unwrap();
        let dav_str = dav_header.to_str().unwrap();
        assert!(dav_str.contains("addressbook"));
    }

    #[tokio::test]
    async fn test_mkaddressbook() {
        let server = setup_carddav_server();

        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_mkaddressbook_already_exists() {
        let server = setup_carddav_server();

        // First create a regular collection
        let req = Request::builder()
            .method("MKCOL")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let _ = server.handle(req).await;

        // Try to create addressbook collection on existing path
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_addressbook_propfind() {
        let server = setup_carddav_server();

        // Create an addressbook collection first
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let _ = server.handle(req).await;

        // PROPFIND request
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <D:resourcetype/>
    <CARD:supported-address-data/>
  </D:prop>
</D:propfind>"#;

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/addressbooks/my-contacts")
            .header("Depth", "0")
            .body(Body::from(propfind_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        // Check that response contains CardDAV properties
        let body_str = resp_to_string(resp).await;
        assert!(body_str.contains("supported-address-data"));
    }

    #[tokio::test]
    async fn test_addressbook_home_set() {
        let server = setup_carddav_server();

        // PROPFIND request for addressbook-home-set on /addressbooks/
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <CARD:addressbook-home-set/>
  </D:prop>
</D:propfind>"#;

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/addressbooks/")
            .header("Depth", "0")
            .body(Body::from(propfind_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        // Check that response contains addressbook-home-set with correct href
        let body_str = resp_to_string(resp).await;
        assert!(
            body_str.contains("addressbook-home-set"),
            "Response body missing 'addressbook-home-set': {}",
            body_str
        );
        assert!(
            body_str.contains("/addressbooks/"),
            "Response body missing '/addressbooks/' href: {}",
            body_str
        );
    }

    #[tokio::test]
    async fn test_vcard_put() {
        let server = setup_carddav_server();

        // Create an addressbook collection first
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert!(resp.status().is_success());

        // PUT a vCard
        let vcard_data = r#"BEGIN:VCARD
VERSION:3.0
UID:test-contact-123@example.com
FN:John Doe
N:Doe;John;;;
EMAIL:john.doe@example.com
TEL:+1-555-123-4567
END:VCARD"#;

        let req = Request::builder()
            .method(Method::PUT)
            .uri("/addressbooks/my-contacts/contact.vcf")
            .header("Content-Type", "text/vcard")
            .body(Body::from(vcard_data))
            .unwrap();

        let resp = server.handle(req).await;
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_addressbook_query_report() {
        let server = setup_carddav_server();

        // Create an addressbook collection
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let _ = server.handle(req).await;

        // Add a contact
        let vcard_data = r#"BEGIN:VCARD
VERSION:3.0
UID:test-contact-123@example.com
FN:John Doe
N:Doe;John;;;
EMAIL:john.doe@example.com
END:VCARD"#;

        let req = Request::builder()
            .method(Method::PUT)
            .uri("/addressbooks/my-contacts/contact.vcf")
            .header("Content-Type", "text/vcard")
            .body(Body::from(vcard_data))
            .unwrap();
        let _ = server.handle(req).await;

        // REPORT addressbook-query
        let report_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<CARD:addressbook-query xmlns:D="DAV:" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <CARD:address-data/>
  </D:prop>
</CARD:addressbook-query>"#;

        let req = Request::builder()
            .method("REPORT")
            .uri("/addressbooks/my-contacts")
            .header("Depth", "1")
            .body(Body::from(report_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let body_str = resp_to_string(resp).await;
        assert!(body_str.contains("address-data"));
        assert!(body_str.contains("John Doe"));
    }

    #[tokio::test]
    async fn test_addressbook_multiget_report() {
        let server = setup_carddav_server();

        // Create an addressbook collection
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let _ = server.handle(req).await;

        // Add first contact
        let vcard_data1 = r#"BEGIN:VCARD
VERSION:3.0
UID:contact-001@example.com
FN:John Doe
N:Doe;John;;;
EMAIL:john.doe@example.com
END:VCARD"#;

        let req = Request::builder()
            .method(Method::PUT)
            .uri("/addressbooks/my-contacts/contact1.vcf")
            .header("Content-Type", "text/vcard")
            .body(Body::from(vcard_data1))
            .unwrap();
        let _ = server.handle(req).await;

        // Add second contact
        let vcard_data2 = r#"BEGIN:VCARD
VERSION:3.0
UID:contact-002@example.com
FN:Jane Smith
N:Smith;Jane;;;
EMAIL:jane.smith@example.com
END:VCARD"#;

        let req = Request::builder()
            .method(Method::PUT)
            .uri("/addressbooks/my-contacts/contact2.vcf")
            .header("Content-Type", "text/vcard")
            .body(Body::from(vcard_data2))
            .unwrap();
        let _ = server.handle(req).await;

        // REPORT addressbook-multiget
        let report_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<CARD:addressbook-multiget xmlns:D="DAV:" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <D:prop>
    <CARD:address-data/>
  </D:prop>
  <D:href>/addressbooks/my-contacts/contact1.vcf</D:href>
  <D:href>/addressbooks/my-contacts/contact2.vcf</D:href>
</CARD:addressbook-multiget>"#;

        let req = Request::builder()
            .method("REPORT")
            .uri("/addressbooks/my-contacts")
            .body(Body::from(report_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let body_str = resp_to_string(resp).await;
        assert!(
            body_str.contains("address-data"),
            "Response body missing 'address-data': {}",
            body_str
        );
        assert!(
            body_str.contains("John Doe"),
            "Response body missing 'John Doe': {}",
            body_str
        );
        assert!(
            body_str.contains("Jane Smith"),
            "Response body missing 'Jane Smith': {}",
            body_str
        );
    }

    #[test]
    fn test_is_vcard_data() {
        let valid_vcard = b"BEGIN:VCARD\nVERSION:3.0\nFN:Test\nEND:VCARD\n";
        assert!(is_vcard_data(valid_vcard));

        let invalid_data = b"This is not vcard data";
        assert!(!is_vcard_data(invalid_data));
    }

    #[test]
    fn test_extract_vcard_uid() {
        // Standard UID
        let vcard_with_uid = "BEGIN:VCARD\nUID:test-123@example.com\nEND:VCARD";
        assert_eq!(
            extract_vcard_uid(vcard_with_uid),
            Some("test-123@example.com".to_string())
        );

        // UID with group prefix (e.g., "item1.UID:...")
        let vcard_grouped_uid = "BEGIN:VCARD\nitem1.UID:grouped-uid@example.com\nEND:VCARD";
        assert_eq!(
            extract_vcard_uid(vcard_grouped_uid),
            Some("grouped-uid@example.com".to_string())
        );

        // UID with parameters
        let vcard_uid_with_params = "BEGIN:VCARD\nUID;VALUE=TEXT:param-uid@example.com\nEND:VCARD";
        assert_eq!(
            extract_vcard_uid(vcard_uid_with_params),
            Some("param-uid@example.com".to_string())
        );

        // UID with group and parameters
        let vcard_grouped_with_params =
            "BEGIN:VCARD\nitem2.UID;VALUE=TEXT:full-uid@example.com\nEND:VCARD";
        assert_eq!(
            extract_vcard_uid(vcard_grouped_with_params),
            Some("full-uid@example.com".to_string())
        );

        // Case insensitive
        let vcard_lowercase = "BEGIN:VCARD\nuid:lowercase@example.com\nEND:VCARD";
        assert_eq!(
            extract_vcard_uid(vcard_lowercase),
            Some("lowercase@example.com".to_string())
        );

        let vcard_without_uid = "BEGIN:VCARD\nFN:Test\nEND:VCARD";
        assert_eq!(extract_vcard_uid(vcard_without_uid), None);
    }

    #[test]
    fn test_extract_vcard_fn() {
        // Standard FN
        let vcard_with_fn = "BEGIN:VCARD\nFN:John Doe\nEND:VCARD";
        assert_eq!(
            extract_vcard_fn(vcard_with_fn),
            Some("John Doe".to_string())
        );

        // FN with group prefix
        let vcard_grouped_fn = "BEGIN:VCARD\nitem1.FN:Jane Smith\nEND:VCARD";
        assert_eq!(
            extract_vcard_fn(vcard_grouped_fn),
            Some("Jane Smith".to_string())
        );

        // FN with parameters
        let vcard_fn_with_params = "BEGIN:VCARD\nFN;CHARSET=UTF-8:Müller\nEND:VCARD";
        assert_eq!(
            extract_vcard_fn(vcard_fn_with_params),
            Some("Müller".to_string())
        );

        let vcard_without_fn = "BEGIN:VCARD\nUID:test\nEND:VCARD";
        assert_eq!(extract_vcard_fn(vcard_without_fn), None);
    }

    #[test]
    fn test_addressbook_properties_default() {
        let props = AddressBookProperties::default();
        assert_eq!(props.max_resource_size, Some(1024 * 1024));
    }

    #[test]
    fn test_validate_vcard_data() {
        let valid_vcard = r#"BEGIN:VCARD
VERSION:3.0
UID:test@example.com
FN:Test Contact
N:Contact;Test;;;
END:VCARD"#;

        assert!(validate_vcard_data(valid_vcard).is_ok());
    }

    #[test]
    fn test_validate_vcard_strict() {
        // Valid vCard with all required properties
        let valid_vcard = r#"BEGIN:VCARD
VERSION:3.0
UID:test@example.com
FN:Test Contact
N:Contact;Test;;;
END:VCARD"#;
        assert!(validate_vcard_strict(valid_vcard).is_ok());

        // Missing FN property
        let missing_fn = r#"BEGIN:VCARD
VERSION:3.0
UID:test@example.com
N:Contact;Test;;;
END:VCARD"#;
        let result = validate_vcard_strict(missing_fn);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("FN"));

        // Missing VERSION property
        let missing_version = r#"BEGIN:VCARD
FN:Test Contact
END:VCARD"#;
        let result = validate_vcard_strict(missing_version);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("VERSION"));
    }
}

#[cfg(not(feature = "carddav"))]
mod carddav_disabled_tests {
    use dav_server::{DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::Request;

    #[tokio::test]
    async fn test_carddav_methods_return_not_implemented() {
        let server = DavHandler::builder()
            .filesystem(MemFs::new())
            .locksystem(FakeLs::new())
            .build_handler();

        // Test MKADDRESSBOOK method
        let req = Request::builder()
            .method("MKADDRESSBOOK")
            .uri("/addressbooks/my-contacts")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_IMPLEMENTED);
    }
}
