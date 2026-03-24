#[cfg(feature = "caldav")]
mod caldav_tests {
    use dav_server::{DavHandler, body::Body, caldav::*, fakels::FakeLs};
    use http::response::Response;
    use http::{Method, Request, StatusCode};

    fn setup_caldav_server() -> DavHandler {
        DavHandler::builder()
            .filesystem(dav_server::memfs::MemFs::new())
            // .filesystem(dav_server::localfs::LocalFs::new("/tmp", true, false, false))
            .locksystem(FakeLs::new())
            .build_handler()
    }

    async fn setup_caldav_server2() -> DavHandler {
        let server = setup_caldav_server();

        let req = Request::builder()
            .method("MKCALENDAR")
            .uri("/calendars/my-calendar")
            .body(Body::empty())
            .unwrap();
        server.handle(req).await;

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

    fn create_ics_data(uid: &str, summary: &str) -> String {
        format!(
            "BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Test//Test//EN
BEGIN:VEVENT
UID:{uid}
DTSTART:20240101T120000Z
DTEND:20240101T130000Z
SUMMARY:{summary}
DESCRIPTION:This is a test event
END:VEVENT
END:VCALENDAR"
        )
    }

    async fn put_ics_data(server: &DavHandler, ics_data: String, uri: &str) -> Response<Body> {
        let req = Request::builder()
            .method(Method::PUT)
            .uri(uri)
            .header("Content-Type", "text/calendar")
            .body(Body::from(ics_data))
            .unwrap();
        server.handle(req).await
    }

    #[tokio::test]
    async fn test_caldav_options() {
        let server = setup_caldav_server();

        let req = Request::builder()
            .method(Method::OPTIONS)
            .uri("/")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::OK);

        let dav_header = resp.headers().get("DAV").unwrap();
        let dav_str = dav_header.to_str().unwrap();
        assert!(dav_str.contains("calendar-access"));
    }

    #[tokio::test]
    async fn test_mkcalendar() {
        let server = setup_caldav_server();

        let req = Request::builder()
            .method("MKCALENDAR")
            .uri("/calendars/my-calendar")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn test_mkcalendar_already_exists() {
        // First create a regular collection
        let server = setup_caldav_server2().await;

        // Try to create calendar collection on existing path
        let req = Request::builder()
            .method("MKCALENDAR")
            .uri("/calendars/my-calendar")
            .body(Body::empty())
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn test_calendar_propfind() {
        let server = setup_caldav_server2().await;

        // PROPFIND request
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<D:propfind xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <D:resourcetype/>
    <D:getetag/>
    <D:supported-report-set/>
    <C:supported-calendar-component-set/>
    <C:supported-calendar-data/>
  </D:prop>
</D:propfind>"#;

        let req = Request::builder()
            .method("PROPFIND")
            .uri("/calendars/my-calendar")
            .header("Depth", "0")
            .body(Body::from(propfind_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        // Check that response contains CalDAV properties
        let body_str = resp_to_string(resp).await;
        println!("{}", body_str);
        assert!(body_str.contains("resourcetype"));
        assert!(body_str.contains("getetag"));
        assert!(body_str.contains("supported-report-set"));
        assert!(body_str.contains("supported-calendar-component-set"));
        assert!(body_str.contains("supported-calendar-data"));
    }

    #[tokio::test]
    async fn test_calendar_event_put() {
        let server = setup_caldav_server2().await;

        // PUT a calendar event
        let ics_data = create_ics_data("test-event-1", "Test Event");
        let resp = put_ics_data(&server, ics_data, "/calendars/my-calendar/event.ics").await;
        assert!(resp.status().is_success());
    }

    #[tokio::test]
    async fn test_calendar_query_report() {
        let server = setup_caldav_server2().await;
        let ics_data = create_ics_data("test-event-1", "Test Event");
        put_ics_data(&server, ics_data, "/calendars/my-calendar/event.ics").await;

        // REPORT calendar-query
        let report_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT"/>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>"#;

        let req = Request::builder()
            .method("REPORT")
            .uri("/calendars/my-calendar")
            .header("Depth", "1")
            .body(Body::from(report_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let body_str = resp_to_string(resp).await;
        assert!(body_str.contains("calendar-data"));
        assert!(body_str.contains("Test Event"));
    }

    #[tokio::test]
    async fn test_calendar_multiget_report() {
        let server = setup_caldav_server2().await;
        let ics_data = create_ics_data("test-event-1", "Test Event0001");
        let resp = put_ics_data(&server, ics_data, "/calendars/my-calendar/event1.ics").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let ics_data = create_ics_data("test-event-2", "Test Event2222");
        let resp = put_ics_data(&server, ics_data, "/calendars/my-calendar/event2.ics").await;
        assert_eq!(resp.status(), StatusCode::CREATED);

        // REPORT calendar-multiget
        let report_body = r#"<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-data/>
  </D:prop>
  <D:href>/calendars/my-calendar/event1.ics</D:href>
  <D:href>/calendars/my-calendar/event2.ics</D:href>
</C:calendar-multiget>"#;

        let req = Request::builder()
            .method("REPORT")
            .uri("/calendars/my-calendar")
            .body(Body::from(report_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let body_str = resp_to_string(resp).await;
        assert!(
            body_str.contains("calendar-data"),
            "Response body missing 'calendar-data': {}",
            body_str
        );
        assert!(
            body_str.contains("Test Event0001"),
            "Response body missing 'Test Event0001': {}",
            body_str
        );
        assert!(
            body_str.contains("Test Event2222"),
            "Response body missing 'Test Event2222': {}",
            body_str
        );
    }

    #[test]
    fn test_is_calendar_data() {
        let valid_ical = b"BEGIN:VCALENDAR\nVERSION:2.0\nEND:VCALENDAR\n";
        assert!(is_calendar_data(valid_ical));

        let invalid_data = b"This is not calendar data";
        assert!(!is_calendar_data(invalid_data));
    }

    #[test]
    fn test_extract_calendar_uid() {
        let ical_with_uid = "BEGIN:VCALENDAR\nUID:test-123@example.com\nEND:VCALENDAR";
        assert_eq!(
            extract_calendar_uid(ical_with_uid),
            Some("test-123@example.com".to_string())
        );

        let ical_without_uid = "BEGIN:VCALENDAR\nSUMMARY:Test\nEND:VCALENDAR";
        assert_eq!(extract_calendar_uid(ical_without_uid), None);
    }

    #[test]
    fn test_validate_calendar_data() {
        let valid_ical = r#"BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Test//Test//EN
BEGIN:VEVENT
UID:test@example.com
DTSTART:20240101T120000Z
DTEND:20240101T130000Z
SUMMARY:Test
END:VEVENT
END:VCALENDAR"#;

        assert!(validate_calendar_data(valid_ical).is_ok());
    }

    #[tokio::test]
    async fn test_calendar_put_match_header() {
        let server = setup_caldav_server2().await;
        let ics_data = create_ics_data("test-event-1", "Test Event");
        let uri = "/calendars/my-calendar/event.ics";
        put_ics_data(&server, ics_data, uri).await;

        // PROPFIND to get etag
        let propfind_body = r#"<?xml version="1.0" encoding="utf-8" ?>
        <D:propfind xmlns:D="DAV:">
        <D:prop>
            <D:getetag/>
        </D:prop>
        </D:propfind>"#;

        let req = Request::builder()
            .method("PROPFIND")
            .uri(uri)
            .header("Depth", "0")
            .body(Body::from(propfind_body))
            .unwrap();

        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::MULTI_STATUS);

        let body_str = resp_to_string(resp).await;
        assert!(body_str.contains("getetag"));
        // A full XML parse might be better, but for a test, we can check for the etag structure.
        // Assuming etag is returned within <D:getetag>...</D:getetag> tags.
        let etag_start = body_str.find("<D:getetag>").unwrap() + "<D:getetag>".len();
        let etag_end = body_str[etag_start..].find("</D:getetag>").unwrap() + etag_start;
        let etag = &body_str[etag_start..etag_end];
        println!("etag: {:#?}", etag);
        assert!(!etag.is_empty());

        // send again, but with MATCH header
        let ics_data = create_ics_data("test-event-1", "Test Event, rename1");
        let req = Request::builder()
            .method(Method::PUT)
            .uri(uri)
            .header("Content-Type", "text/calendar")
            .header("if-match", etag) // Use the obtained etag
            .body(Body::from(ics_data))
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), StatusCode::NO_CONTENT); // For an update, it should be NO_CONTENT
    }
}

#[cfg(not(feature = "caldav"))]
mod caldav_disabled_tests {
    use dav_server::{DavHandler, body::Body, fakels::FakeLs, memfs::MemFs};
    use http::Request;

    #[tokio::test]
    async fn test_caldav_methods_return_not_implemented() {
        let server = DavHandler::builder()
            .filesystem(MemFs::new())
            .locksystem(FakeLs::new())
            .build_handler();

        // Test REPORT method - only returns NOT_IMPLEMENTED when carddav is also disabled
        #[cfg(not(feature = "carddav"))]
        {
            let req = Request::builder()
                .method("REPORT")
                .uri("/")
                .body(Body::empty())
                .unwrap();
            let resp = server.handle(req).await;
            assert_eq!(resp.status(), http::StatusCode::NOT_IMPLEMENTED);
        }

        // Test MKCALENDAR method
        let req = Request::builder()
            .method("MKCALENDAR")
            .uri("/calendars/my-calendar")
            .body(Body::empty())
            .unwrap();
        let resp = server.handle(req).await;
        assert_eq!(resp.status(), http::StatusCode::NOT_IMPLEMENTED);
    }
}
