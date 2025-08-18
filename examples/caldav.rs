//! CalDAV server example
//!
//! This example demonstrates how to set up a CalDAV server using the dav-server library.
//! CalDAV is an extension of WebDAV for calendar data management.
//!
//! Usage:
//!   cargo run --example caldav --features caldav
//!
//! The server will be available at http://localhost:8080
//! You can connect to it using CalDAV clients like Thunderbird, Apple Calendar, etc.

use dav_server::{DavHandler, DavMethodSet, fakels::FakeLs, memfs::MemFs};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use std::{convert::Infallible, net::SocketAddr};
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::init();

    let addr: SocketAddr = ([127, 0, 0, 1], 8080).into();

    // Set up the DAV handler with CalDAV support
    // Note: Using MemFs for this example because it supports the property operations
    // needed for CalDAV collections. For production use, you'd want a filesystem
    // implementation that persists properties to disk (e.g., using extended attributes
    // or sidecar files).
    let dav_server = DavHandler::builder()
        .filesystem(MemFs::new())
        .locksystem(FakeLs::new())
        .strip_prefix("")
        .methods(DavMethodSet::all())
        .build_handler();

    let listener = TcpListener::bind(addr).await?;

    println!("CalDAV server listening on {}", addr);
    println!("Calendar collections can be accessed at http://{}", addr);
    println!();
    println!("NOTE: This example uses in-memory storage. Data will be lost when the server stops.");
    println!();
    println!("To create a calendar collection, use:");
    println!("  curl -i -X MKCALENDAR http://{}/my-calendar/", addr);
    println!();
    println!("To add a calendar event, use:");
    println!("  curl -i -X PUT http://{}/my-calendar/event1.ics \\", addr);
    println!("    -H 'Content-Type: text/calendar' \\");
    println!("    --data-binary @event.ics");
    println!();
    println!("Example event.ics content:");
    println!("BEGIN:VCALENDAR");
    println!("VERSION:2.0");
    println!("PRODID:-//Example Corp//CalDAV Client//EN");
    println!("BEGIN:VEVENT");
    println!("UID:12345@example.com");
    println!("DTSTART:20240101T120000Z");
    println!("DTEND:20240101T130000Z");
    println!("SUMMARY:New Year Meeting");
    println!("DESCRIPTION:Planning meeting for the new year");
    println!("END:VEVENT");
    println!("END:VCALENDAR");

    // Start the server loop
    loop {
        let (stream, _) = listener.accept().await?;
        let dav_server = dav_server.clone();

        let io = TokioIo::new(stream);

        tokio::task::spawn(async move {
            if let Err(err) = http1::Builder::new()
                .serve_connection(
                    io,
                    service_fn({
                        move |req| {
                            let dav_server = dav_server.clone();
                            async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
                        }
                    }),
                )
                .await
            {
                eprintln!("Failed serving connection: {err:?}");
            }
        });
    }
}

#[cfg(not(feature = "caldav"))]
fn main() {
    eprintln!("This example requires the 'caldav' feature to be enabled.");
    eprintln!("Run with: cargo run --example caldav --features caldav");
    std::process::exit(1);
}
