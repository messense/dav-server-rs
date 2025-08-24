#[cfg(feature = "caldav")]
use chrono::Utc;
use futures_util::StreamExt;
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};
use std::io::Cursor;
use xml::reader::{EventReader, XmlEvent};
use xml::writer::{EventWriter, XmlEvent as XmlWEvent};
use xmltree::{Element, XMLNode};

use crate::body::Body;
use crate::errors::*;
use crate::fs::*;
use crate::util::MemBuffer;
use crate::{DavInner, DavResult};

#[cfg(feature = "caldav")]
use crate::caldav::*;
#[cfg(feature = "caldav")]
use crate::davpath::DavPath;

impl<C: Clone + Send + Sync + 'static> DavInner<C> {
    /// Handle CalDAV REPORT method
    pub(crate) async fn handle_report(
        &self,
        req: &Request<()>,
        body: &[u8],
    ) -> DavResult<Response<Body>> {
        let path = self.path(req);

        // Parse the REPORT request body
        let report_type = self.parse_report_request(body)?;

        match report_type {
            CalDavReportType::CalendarQuery(query) => {
                self.handle_calendar_query(&path, query).await
            }
            CalDavReportType::CalendarMultiget { hrefs } => {
                self.handle_calendar_multiget(&path, hrefs).await
            }
            CalDavReportType::FreeBusyQuery { time_range } => {
                self.handle_freebusy_query(&path, time_range).await
            }
        }
    }

    /// Handle CalDAV MKCALENDAR method
    pub(crate) async fn handle_mkcalendar(
        &self,
        req: &Request<()>,
        _body: &[u8],
    ) -> DavResult<Response<Body>> {
        let path = self.path(req);

        // Check if the collection already exists
        if self.fs.metadata(&path, &self.credentials).await.is_ok() {
            return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
        }

        // Create the calendar collection
        self.fs.create_dir(&path, &self.credentials).await?;

        // Set calendar-specific properties to identify this as a calendar collection
        self.set_calendar_properties(&path).await?;

        let mut resp = Response::new(Body::empty());
        *resp.status_mut() = StatusCode::CREATED;
        resp.headers_mut().typed_insert(headers::ContentLength(0));

        Ok(resp)
    }

    #[cfg(feature = "caldav")]
    fn parse_report_request(&self, body: &[u8]) -> DavResult<CalDavReportType> {
        if body.is_empty() {
            return Err(DavError::StatusClose(StatusCode::BAD_REQUEST));
        }

        let cursor = Cursor::new(body);
        let parser = EventReader::new(cursor);
        let mut elements: Vec<Element> = Vec::new();
        let mut current_element: Option<Element> = None;
        let mut element_stack: Vec<Element> = Vec::new();

        for event in parser {
            match event {
                Ok(XmlEvent::StartElement {
                    name,
                    attributes,
                    namespace,
                }) => {
                    let mut elem = Element::new(&name.local_name);
                    if let Some(prefix) = name.prefix {
                        if let Some(uri) = namespace.get(&prefix) {
                            elem.namespace = Some(uri.to_string());
                        }
                    }

                    for attr in attributes {
                        elem.attributes.insert(attr.name.local_name, attr.value);
                    }

                    if let Some(parent) = current_element.take() {
                        element_stack.push(parent);
                    }
                    current_element = Some(elem);
                }
                Ok(XmlEvent::EndElement { .. }) => {
                    if let Some(elem) = current_element.take() {
                        if let Some(mut parent) = element_stack.pop() {
                            parent.children.push(XMLNode::Element(elem));
                            current_element = Some(parent);
                        } else {
                            elements.push(elem);
                        }
                    }
                }
                Ok(XmlEvent::Characters(text)) => {
                    if let Some(ref mut elem) = current_element {
                        elem.children.push(XMLNode::Text(text));
                    }
                }
                _ => {}
            }
        }

        // Parse the root element to determine report type
        if let Some(root) = elements.first() {
            match root.name.as_str() {
                "calendar-query" => {
                    let query = self.parse_calendar_query(root)?;
                    Ok(CalDavReportType::CalendarQuery(query))
                }
                "calendar-multiget" => {
                    let hrefs = self.parse_calendar_multiget(root)?;
                    Ok(CalDavReportType::CalendarMultiget { hrefs })
                }
                "free-busy-query" => {
                    let time_range = self.parse_freebusy_query(root)?;
                    Ok(CalDavReportType::FreeBusyQuery { time_range })
                }
                _ => Err(DavError::StatusClose(StatusCode::BAD_REQUEST)),
            }
        } else {
            Err(DavError::StatusClose(StatusCode::BAD_REQUEST))
        }
    }

    #[cfg(feature = "caldav")]
    fn parse_calendar_query(&self, root: &Element) -> DavResult<CalendarQuery> {
        let mut query = CalendarQuery {
            comp_filter: None,
            time_range: None,
            properties: Vec::new(),
        };

        for child in &root.children {
            if let XMLNode::Element(elem) = child {
                match elem.name.as_str() {
                    "filter" => {
                        // Parse comp-filter elements
                        for filter_child in &elem.children {
                            if let XMLNode::Element(filter_elem) = filter_child {
                                if filter_elem.name == "comp-filter" {
                                    query.comp_filter =
                                        Some(self.parse_component_filter(filter_elem)?);
                                }
                            }
                        }
                    }
                    "prop" => {
                        // Parse requested properties
                        for prop_child in &elem.children {
                            if let XMLNode::Element(prop_elem) = prop_child {
                                query.properties.push(prop_elem.name.clone());
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        Ok(query)
    }

    #[cfg(feature = "caldav")]
    fn parse_component_filter(&self, elem: &Element) -> DavResult<ComponentFilter> {
        let name = elem
            .attributes
            .get("name")
            .ok_or(DavError::StatusClose(StatusCode::BAD_REQUEST))?
            .clone();

        let mut filter = ComponentFilter {
            name,
            is_not_defined: false,
            time_range: None,
            prop_filters: Vec::new(),
            comp_filters: Vec::new(),
        };

        for child in &elem.children {
            if let XMLNode::Element(child_elem) = child {
                match child_elem.name.as_str() {
                    "is-not-defined" => {
                        filter.is_not_defined = true;
                    }
                    "time-range" => {
                        filter.time_range = Some(self.parse_time_range(child_elem)?);
                    }
                    "prop-filter" => {
                        filter
                            .prop_filters
                            .push(self.parse_property_filter(child_elem)?);
                    }
                    "comp-filter" => {
                        filter
                            .comp_filters
                            .push(self.parse_component_filter(child_elem)?);
                    }
                    _ => {}
                }
            }
        }

        Ok(filter)
    }

    #[cfg(feature = "caldav")]
    fn parse_property_filter(&self, elem: &Element) -> DavResult<PropertyFilter> {
        let name = elem
            .attributes
            .get("name")
            .ok_or(DavError::StatusClose(StatusCode::BAD_REQUEST))?
            .clone();

        let mut filter = PropertyFilter {
            name,
            is_not_defined: false,
            text_match: None,
            time_range: None,
            param_filters: Vec::new(),
        };

        for child in &elem.children {
            if let XMLNode::Element(child_elem) = child {
                match child_elem.name.as_str() {
                    "is-not-defined" => {
                        filter.is_not_defined = true;
                    }
                    "time-range" => {
                        filter.time_range = Some(self.parse_time_range(child_elem)?);
                    }
                    "text-match" => {
                        filter.text_match = Some(self.parse_text_match(child_elem)?);
                    }
                    _ => {}
                }
            }
        }

        Ok(filter)
    }

    #[cfg(feature = "caldav")]
    fn parse_time_range(&self, elem: &Element) -> DavResult<TimeRange> {
        Ok(TimeRange {
            start: elem.attributes.get("start").cloned(),
            end: elem.attributes.get("end").cloned(),
        })
    }

    #[cfg(feature = "caldav")]
    fn parse_text_match(&self, elem: &Element) -> DavResult<TextMatch> {
        let text = elem
            .children
            .iter()
            .find_map(|child| {
                if let XMLNode::Text(text) = child {
                    Some(text.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();

        Ok(TextMatch {
            text,
            collation: elem.attributes.get("collation").cloned(),
            negate_condition: elem
                .attributes
                .get("negate-condition")
                .map(|v| v == "yes")
                .unwrap_or(false),
        })
    }

    #[cfg(feature = "caldav")]
    fn parse_calendar_multiget(&self, root: &Element) -> DavResult<Vec<String>> {
        let mut hrefs = Vec::new();

        for child in &root.children {
            if let XMLNode::Element(elem) = child {
                if elem.name == "href" {
                    for href_child in &elem.children {
                        if let XMLNode::Text(href) = href_child {
                            hrefs.push(href.clone());
                        }
                    }
                }
            }
        }

        Ok(hrefs)
    }

    #[cfg(feature = "caldav")]
    fn parse_freebusy_query(&self, root: &Element) -> DavResult<TimeRange> {
        for child in &root.children {
            if let XMLNode::Element(elem) = child {
                if elem.name == "time-range" {
                    return self.parse_time_range(elem);
                }
            }
        }

        Err(DavError::StatusClose(StatusCode::BAD_REQUEST))
    }

    #[cfg(feature = "caldav")]
    async fn handle_calendar_query(
        &self,
        path: &DavPath,
        query: CalendarQuery,
    ) -> DavResult<Response<Body>> {
        // Get directory listing
        let stream = self
            .fs
            .read_dir(path, ReadDirMeta::Data, &self.credentials)
            .await?;
        let mut results = Vec::new();

        let items: Vec<_> = stream.collect().await;

        for item in items {
            match item {
                Ok(dirent) => {
                    let mut item_path = path.clone();
                    item_path.push_segment(&dirent.name());

                    // Check if this is a calendar resource, and append content to result
                    if let Ok(mut file) = self
                        .fs
                        .open(&item_path, OpenOptions::read(), &self.credentials)
                        .await
                    {
                        let metadata = file.metadata().await?;

                        if let Ok(data) = file.read_bytes(metadata.len() as usize).await {
                            if is_calendar_data(&data) {
                                let content = String::from_utf8_lossy(&data);

                                if self.matches_query(&content, &query) {
                                    results.push((item_path.clone(), content.to_string()));
                                }
                            }
                        }
                    }
                }
                Err(_) => continue,
            }
        }

        // Generate multistatus response
        self.generate_calendar_query_response(results).await
    }

    #[cfg(feature = "caldav")]
    async fn handle_calendar_multiget(
        &self,
        _: &DavPath,
        hrefs: Vec<String>,
    ) -> DavResult<Response<Body>> {
        let mut results = Vec::new();

        for href in hrefs {
            if let Ok(item_path) = DavPath::from_str_and_prefix(&href, &self.prefix) {
                if let Ok(mut file) = self
                    .fs
                    .open(&item_path, OpenOptions::read(), &self.credentials)
                    .await
                {
                    let metadata = file.metadata().await?;

                    if let Ok(data) = &file.read_bytes(metadata.len() as usize).await {
                        if is_calendar_data(&data) {
                            let content = String::from_utf8_lossy(data);
                            results.push((item_path, content.to_string()));
                        }
                    }
                }
            }
        }

        self.generate_calendar_query_response(results).await
    }

    #[cfg(feature = "caldav")]
    async fn handle_freebusy_query(
        &self,
        _: &DavPath,
        time_range: TimeRange,
    ) -> DavResult<Response<Body>> {
        //TODO: freebusy implementation
        // For now, return an empty freebusy response
        // A full implementation would analyze calendar events and generate freebusy information

        let freebusy_data = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//DAV-SERVER//CalDAV//EN\r\n\
             BEGIN:VFREEBUSY\r\nUID:{}\r\nDTSTAMP:{}Z\r\n\
             DTSTART:{}\r\nDTEND:{}\r\n\
             END:VFREEBUSY\r\nEND:VCALENDAR\r\n",
            uuid::Uuid::new_v4(),
            Utc::now().format("%Y%m%dT%H%M%S"),
            time_range.start.as_deref().unwrap_or("20000101T000000Z"),
            time_range.end.as_deref().unwrap_or("20991231T235959Z")
        );

        let mut resp = Response::new(Body::from(freebusy_data));
        resp.headers_mut().insert(
            "content-type",
            "text/calendar; charset=utf-8".parse().unwrap(),
        );
        Ok(resp)
    }

    #[cfg(feature = "caldav")]
    fn matches_query(&self, content: &str, query: &CalendarQuery) -> bool {
        // Simple implementation - a full implementation would parse the iCalendar
        // and apply all the filters properly

        if let Some(ref comp_filter) = query.comp_filter {
            if !content.contains(&format!("BEGIN:{}", comp_filter.name)) {
                return false;
            }
        }

        true
    }

    #[cfg(feature = "caldav")]
    async fn generate_calendar_query_response(
        &self,
        results: Vec<(DavPath, String)>,
    ) -> DavResult<Response<Body>> {
        let mut buffer = MemBuffer::new();
        let mut writer = EventWriter::new(&mut buffer);

        writer.write(
            XmlWEvent::start_element("multistatus")
                .ns("D", "DAV:")
                .ns("C", NS_CALDAV_URI),
        )?;

        for (href, calendar_data) in results {
            writer.write(XmlWEvent::start_element("response").ns("D", "DAV:"))?;

            writer.write(XmlWEvent::start_element("href").ns("D", "DAV:"))?;
            writer.write(XmlWEvent::characters(&href.as_url_string()))?;
            writer.write(XmlWEvent::end_element())?;

            writer.write(XmlWEvent::start_element("propstat").ns("D", "DAV:"))?;
            writer.write(XmlWEvent::start_element("prop").ns("D", "DAV:"))?;

            // Add calendar-data property
            writer.write(XmlWEvent::start_element("calendar-data").ns("C", NS_CALDAV_URI))?;
            writer.write(XmlWEvent::characters(&calendar_data))?;
            writer.write(XmlWEvent::end_element())?;

            writer.write(XmlWEvent::end_element())?; // prop

            writer.write(XmlWEvent::start_element("status").ns("D", "DAV:"))?;
            writer.write(XmlWEvent::characters("HTTP/1.1 200 OK"))?;
            writer.write(XmlWEvent::end_element())?;

            writer.write(XmlWEvent::end_element())?; // propstat
            writer.write(XmlWEvent::end_element())?; // response
        }

        writer.write(XmlWEvent::end_element())?; // multistatus

        let xml_data = buffer.take();
        let mut resp = Response::new(Body::from(xml_data));
        resp.headers_mut().insert(
            "content-type",
            "application/xml; charset=utf-8".parse().unwrap(),
        );
        *resp.status_mut() = StatusCode::MULTI_STATUS;

        Ok(resp)
    }

    // #[cfg(feature = "caldav")]
    // fn parse_mkcalendar_request(&self, body: &[u8]) -> DavResult<CalendarProperties> {
    //     // Parse the MKCALENDAR request body for calendar properties
    //     // For now, return default properties

    //     if body.is_empty() {
    //         Err(DavError::StatusClose(StatusCode::BAD_REQUEST))
    //     } else {
    //         Ok(CalendarProperties::default())
    //     }
    // }

    #[cfg(feature = "caldav")]
    /// Save Calendar data to DavFile
    ///
    /// Set calendar-specific properties to identify a directory as a calendar collection
    async fn set_calendar_properties(&self, path: &DavPath) -> DavResult<()> {
        use crate::fs::DavProp;

        // Set supported-calendar-component-set property
        let comp_set_prop = DavProp {
            name: "supported-calendar-component-set".to_string(),
            prefix: Some("C".to_string()),
            namespace: Some(NS_CALDAV_URI.to_string()),
            xml: Some(b"<C:supported-calendar-component-set xmlns:C=\"urn:ietf:params:xml:ns:caldav\"><C:comp name=\"VEVENT\"/><C:comp name=\"VTODO\"/><C:comp name=\"VJOURNAL\"/><C:comp name=\"VFREEBUSY\"/></C:supported-calendar-component-set>".to_vec()),
        };

        // Set calendar-description property
        let desc_prop = DavProp {
            name: "calendar-description".to_string(),
            prefix: Some("C".to_string()),
            namespace: Some(NS_CALDAV_URI.to_string()),
            xml: Some(b"<C:calendar-description xmlns:C=\"urn:ietf:params:xml:ns:caldav\">Calendar Collection</C:calendar-description>".to_vec()),
        };

        // Save properties using patch_props (true = set property)
        let patch = vec![(true, comp_set_prop), (true, desc_prop)];
        self.fs.patch_props(path, patch, &self.credentials).await?;

        Ok(())
    }
}
