//! CalDAV (Calendaring Extensions to WebDAV) support
//!
//! This module provides CalDAV functionality on top of the base WebDAV implementation.
//! CalDAV is defined in RFC 4791 and provides standardized access to calendar data
//! using the iCalendar format.

#[cfg(feature = "caldav")]
use icalendar::Calendar;
use xmltree::Element;

// CalDAV XML namespaces
pub const NS_CALDAV_URI: &str = "urn:ietf:params:xml:ns:caldav";
pub const NS_CALENDARSERVER_URI: &str = "http://calendarserver.org/ns/";

// CalDAV property names
pub const CALDAV_PROPERTIES: &[&str] = &[
    "C:calendar-description",
    "C:calendar-timezone",
    "C:supported-calendar-component-set",
    "C:supported-calendar-data",
    "C:max-resource-size",
    "C:min-date-time",
    "C:max-date-time",
    "C:max-instances",
    "C:max-attendees-per-instance",
    "C:calendar-home-set",
    "C:calendar-user-address-set",
    "C:schedule-inbox-URL",
    "C:schedule-outbox-URL",
];

/// CalDAV resource types
#[derive(Debug, Clone, PartialEq)]
pub enum CalDavResourceType {
    Calendar,
    ScheduleInbox,
    ScheduleOutbox,
    CalendarObject,
    Regular,
}

/// CalDAV component types supported in a calendar collection
#[derive(Debug, Clone, PartialEq)]
pub enum CalendarComponentType {
    VEvent,
    VTodo,
    VJournal,
    VFreeBusy,
    VTimezone,
    VAlarm,
}

impl CalendarComponentType {
    pub fn as_str(&self) -> &'static str {
        match self {
            CalendarComponentType::VEvent => "VEVENT",
            CalendarComponentType::VTodo => "VTODO",
            CalendarComponentType::VJournal => "VJOURNAL",
            CalendarComponentType::VFreeBusy => "VFREEBUSY",
            CalendarComponentType::VTimezone => "VTIMEZONE",
            CalendarComponentType::VAlarm => "VALARM",
        }
    }
}

/// CalDAV calendar collection properties
#[derive(Debug, Clone)]
pub struct CalendarProperties {
    pub description: Option<String>,
    pub timezone: Option<String>,
    pub supported_components: Vec<CalendarComponentType>,
    pub max_resource_size: Option<u64>,
    pub color: Option<String>,
    pub display_name: Option<String>,
}

impl Default for CalendarProperties {
    fn default() -> Self {
        Self {
            description: None,
            timezone: None,
            supported_components: vec![
                CalendarComponentType::VEvent,
                CalendarComponentType::VTodo,
                CalendarComponentType::VJournal,
                CalendarComponentType::VFreeBusy,
            ],
            max_resource_size: Some(1024 * 1024), // 1MB default
            color: None,
            display_name: None,
        }
    }
}

/// Calendar query filters for REPORT requests
#[derive(Debug, Clone)]
pub struct CalendarQuery {
    pub comp_filter: Option<ComponentFilter>,
    pub time_range: Option<TimeRange>,
    pub properties: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ComponentFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub time_range: Option<TimeRange>,
    pub prop_filters: Vec<PropertyFilter>,
    pub comp_filters: Vec<ComponentFilter>,
}

#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub text_match: Option<TextMatch>,
    pub time_range: Option<TimeRange>,
    pub param_filters: Vec<ParameterFilter>,
}

#[derive(Debug, Clone)]
pub struct ParameterFilter {
    pub name: String,
    pub is_not_defined: bool,
    pub text_match: Option<TextMatch>,
}

#[derive(Debug, Clone)]
pub struct TextMatch {
    pub text: String,
    pub collation: Option<String>,
    pub negate_condition: bool,
}

#[derive(Debug, Clone)]
pub struct TimeRange {
    pub start: Option<String>, // ISO 8601 format
    pub end: Option<String>,   // ISO 8601 format
}

/// CalDAV REPORT request types
#[derive(Debug, Clone)]
pub enum CalDavReportType {
    CalendarQuery(CalendarQuery),
    CalendarMultiget { hrefs: Vec<String> },
    FreeBusyQuery { time_range: TimeRange },
}

/// Helper functions for CalDAV XML generation
pub fn create_supported_calendar_component_set(components: &[CalendarComponentType]) -> Element {
    let mut elem = Element::new("supported-calendar-component-set");
    elem.namespace = Some(NS_CALDAV_URI.to_string());

    for comp in components {
        let mut comp_elem = Element::new("comp");
        comp_elem.namespace = Some(NS_CALDAV_URI.to_string());
        comp_elem
            .attributes
            .insert("name".to_string(), comp.as_str().to_string());
        elem.children.push(xmltree::XMLNode::Element(comp_elem));
    }

    elem
}

pub fn create_supported_calendar_data() -> Element {
    let mut elem = Element::new("supported-calendar-data");
    elem.namespace = Some(NS_CALDAV_URI.to_string());

    let mut calendar_data = Element::new("calendar-data");
    calendar_data.namespace = Some(NS_CALDAV_URI.to_string());
    calendar_data
        .attributes
        .insert("content-type".to_string(), "text/calendar".to_string());
    calendar_data
        .attributes
        .insert("version".to_string(), "2.0".to_string());

    elem.children.push(xmltree::XMLNode::Element(calendar_data));
    elem
}

pub fn create_calendar_home_set(path: &str) -> Element {
    let mut elem = Element::new("calendar-home-set");
    elem.namespace = Some(NS_CALDAV_URI.to_string());

    let mut href = Element::new("href");
    href.namespace = Some("DAV:".to_string());
    href.children.push(xmltree::XMLNode::Text(path.to_string()));

    elem.children.push(xmltree::XMLNode::Element(href));
    elem
}

/// Check if a resource is a calendar collection based on resource type
pub fn is_calendar_collection(resource_type: &[Element]) -> bool {
    resource_type
        .iter()
        .any(|elem| elem.name == "calendar" && elem.namespace.as_deref() == Some(NS_CALDAV_URI))
}

/// Check if content appears to be iCalendar data
pub fn is_calendar_data(content: &[u8]) -> bool {
    content.starts_with(b"BEGIN:VCALENDAR")
        && (content.ends_with(b"END:VCALENDAR") || content.ends_with(b"END:VCALENDAR\n"))
}

#[cfg(feature = "caldav")]
/// Validate iCalendar data using the icalendar crate
pub fn validate_calendar_data(content: &str) -> Result<Calendar, String> {
    content
        .parse::<Calendar>()
        .map_err(|e| format!("Invalid iCalendar data: {}", e))
}

#[cfg(not(feature = "caldav"))]
/// Stub implementation when caldav feature is disabled
pub fn validate_calendar_data(_content: &str) -> Result<(), String> {
    Err("CalDAV feature not enabled".to_string())
}

/// Extract the UID from calendar data
pub fn extract_calendar_uid(content: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("UID:") {
            return Some(line[4..].to_string());
        }
    }
    None
}

/// Generate a simple calendar collection resource type XML
pub fn calendar_resource_type() -> Vec<Element> {
    let mut collection = Element::new("collection");
    collection.namespace = Some("DAV:".to_string());

    let mut calendar = Element::new("calendar");
    calendar.namespace = Some(NS_CALDAV_URI.to_string());

    vec![collection, calendar]
}

/// Generate schedule inbox resource type XML
pub fn schedule_inbox_resource_type() -> Vec<Element> {
    let mut collection = Element::new("collection");
    collection.namespace = Some("DAV:".to_string());

    let mut schedule_inbox = Element::new("schedule-inbox");
    schedule_inbox.namespace = Some(NS_CALDAV_URI.to_string());

    vec![collection, schedule_inbox]
}

/// Generate schedule outbox resource type XML
pub fn schedule_outbox_resource_type() -> Vec<Element> {
    let mut collection = Element::new("collection");
    collection.namespace = Some("DAV:".to_string());

    let mut schedule_outbox = Element::new("schedule-outbox");
    schedule_outbox.namespace = Some(NS_CALDAV_URI.to_string());

    vec![collection, schedule_outbox]
}
