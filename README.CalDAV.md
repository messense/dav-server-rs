# CalDAV Support in dav-server

This document describes the CalDAV (Calendaring Extensions to WebDAV) support in the dav-server library.

## Overview

CalDAV is an extension of WebDAV that provides a standard way to access and manage calendar data over HTTP. It's defined in [RFC 4791](https://tools.ietf.org/html/rfc4791) and allows calendar clients to:

- Create and manage calendar collections
- Store and retrieve calendar events, tasks, and journals
- Query calendars with complex filters
- Synchronize calendar data between clients and servers

## Features

The CalDAV implementation in dav-server includes:

- **Calendar Collections**: A directory that functions as a calendar, containing one `.ics` file for each event.
- **MKCALENDAR Method**: Create new calendar collection
- **REPORT Method**: Query calendar data with filters
- **CalDAV Properties**: Calendar-specific WebDAV properties
- **iCalendar Support**: Parse and validate iCalendar data
- **Time Range Queries**: Filter events by date/time ranges
- **Component Filtering**: Filter by calendar component types (VEVENT, VTODO, etc.)

## Enabling CalDAV

CalDAV support is available as an optional cargo feature:

```toml
[dependencies]
dav-server = { version = "0.8", features = ["caldav"] }
```

This adds the following dependencies:
- `icalendar`: For parsing and validating iCalendar data
- `chrono`: For date/time handling

## Quick Start

Here's a basic CalDAV server setup:

```rust
use dav_server::{DavHandler, fakels::FakeLs, localfs::LocalFs};

let server = DavHandler::builder()
    .filesystem(LocalFs::new("/calendars", false, false, false))
    .locksystem(FakeLs::new())
    .build_handler();
```

## CalDAV Methods

### MKCALENDAR

Creates a new calendar collection:

```bash
curl -X MKCALENDAR http://localhost:8080/calendars/my-calendar/
```

With properties:

```bash
curl -X MKCALENDAR http://localhost:8080/calendars/my-calendar/ \
  -H "Content-Type: application/xml" \
  --data '<?xml version="1.0" encoding="utf-8" ?>
<C:mkcalendar xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:set>
    <D:prop>
      <D:displayname>My Calendar</D:displayname>
      <C:calendar-description>Personal calendar</C:calendar-description>
    </D:prop>
  </D:set>
</C:mkcalendar>'
```

### REPORT

Query calendar data:

#### Calendar Query

```bash
curl -X REPORT http://localhost:8080/calendars/my-calendar/ \
  -H "Content-Type: application/xml" \
  -H "Depth: 1" \
  --data '<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-data/>
  </D:prop>
  <C:filter>
    <C:comp-filter name="VCALENDAR">
      <C:comp-filter name="VEVENT">
        <C:time-range start="20240101T000000Z" end="20241231T235959Z"/>
      </C:comp-filter>
    </C:comp-filter>
  </C:filter>
</C:calendar-query>'
```

#### Calendar Multiget

```bash
curl -X REPORT http://localhost:8080/calendars/my-calendar/ \
  -H "Content-Type: application/xml" \
  --data '<?xml version="1.0" encoding="utf-8" ?>
<C:calendar-multiget xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav">
  <D:prop>
    <C:calendar-data/>
  </D:prop>
  <D:href>/my-calendar/event1.ics</D:href>
  <D:href>/my-calendar/event2.ics</D:href>
</C:calendar-multiget>'
```

## CalDAV Properties

The implementation supports standard CalDAV properties:

### Collection Properties

- `calendar-description`: Human-readable description
- `calendar-timezone`: Default timezone for the calendar
- `supported-calendar-component-set`: Supported component types (VEVENT, VTODO, etc.)
- `supported-calendar-data`: Supported calendar data formats
- `max-resource-size`: Maximum size for calendar resources

### Principal Properties

- `calendar-home-set`: URL of the user's calendar home collection
- `calendar-user-address-set`: Calendar user's addresses
- `schedule-inbox-URL`: URL for scheduling messages
- `schedule-outbox-URL`: URL for outgoing scheduling

## Working with Calendar Data

### Adding Events

Store iCalendar data using PUT:

```bash
curl -X PUT http://localhost:8080/calendars/my-calendar/event.ics \
  -H "Content-Type: text/calendar" \
  --data 'BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//Example Corp//CalDAV Client//EN
BEGIN:VEVENT
UID:12345@example.com
DTSTART:20240101T120000Z
DTEND:20240101T130000Z
SUMMARY:New Year Meeting
DESCRIPTION:Planning meeting for the new year
END:VEVENT
END:VCALENDAR'
```

### Retrieving Events

Use GET to retrieve individual calendar resources:

```bash
curl http://localhost:8080/calendars/my-calendar/event.ics
```

## Client Compatibility

The CalDAV implementation has been tested with:

- **Thunderbird**: Full support for calendar sync
- **Apple Calendar**: Compatible with basic operations
- **CalDAV-Sync (Android)**: Works with standard CalDAV features
- **Evolution**: Support for calendar collections and events

## Limitations

Current limitations include:

- No scheduling support (iTIP/iMIP)
- Limited calendar-user-principal support
- No calendar sharing or ACL support
- Basic time zone handling
- No recurring event expansion in queries

## Example Applications
These calendar server examples lacks authentication and does not support user-specific access. The default FileSystems can only create collections on the path "/calendars".  
For a production environment, you should implement the GuardedFileSystem for better security and user management.

### Calendar Server

```rust
use dav_server::{DavHandler, fakels::FakeLs, localfs::LocalFs};
use std::net::SocketAddr;

#[tokio::main]
async fn main() {
    let server = DavHandler::builder()
        .filesystem(LocalFs::new("/calendars", false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    // Serve on port 8080
    // Calendars accessible at http://localhost:8080/calendars/
}
```

### Multi-tenant Calendar Service

```rust
use dav_server::{DavHandler, memfs::MemFs, memls::MemLs};

// Use in-memory filesystem for demonstration
let server = DavHandler::builder()
    .filesystem(MemFs::new())
    .locksystem(MemLs::new())
    .principal("/principals/user1/")
    .build_handler();
```

## Testing

Run CalDAV tests with:

```bash
cargo test --features caldav caldav_tests
```

Run the CalDAV example:

```bash
cargo run --example caldav --features caldav
```

## Standards Compliance

This implementation follows:

- [RFC 4791](https://tools.ietf.org/html/rfc4791) - Calendaring Extensions to WebDAV (CalDAV)
- [RFC 5545](https://tools.ietf.org/html/rfc5545) - Internet Calendaring and Scheduling Core Object Specification (iCalendar)
- [RFC 4918](https://tools.ietf.org/html/rfc4918) - HTTP Extensions for Web Distributed Authoring and Versioning (WebDAV)

## Contributing

Contributions to improve CalDAV support are welcome. Areas for enhancement include:

- Scheduling support (iTIP)
- Additional client compatibility testing