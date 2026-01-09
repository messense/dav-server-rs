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

use chrono::Datelike;
use dav_server::caldav::DEFAULT_CALDAV_DIRECTORY;
use dav_server::fs::DavFileSystem;
use dav_server::{
    DavHandler, DavMethodSet, body::Body, davpath::DavPath, fakels::FakeLs, memfs::MemFs,
};
use hyper::{header::HeaderValue, server::conn::http1, service::service_fn};
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

    // We expect a directory for CalDAV
    let filesystem = MemFs::new();
    filesystem
        .create_dir(&DavPath::new(DEFAULT_CALDAV_DIRECTORY)?)
        .await?;

    let dav_server = DavHandler::builder()
        .filesystem(filesystem)
        .locksystem(FakeLs::new())
        .methods(DavMethodSet::all())
        .build_handler();

    let listener = TcpListener::bind(addr).await?;

    println!("CalDAV server listening on http://{}", addr);
    println!(
        "Calendar collections can be accessed at http://{}{}",
        addr, DEFAULT_CALDAV_DIRECTORY
    );
    println!();
    println!("NOTE: This example uses in-memory storage. Data will be lost when the server stops.");
    println!();
    println!("To create a calendar collection, use:");
    println!(
        "  curl -i -X MKCALENDAR http://{}{}/my-calendar/",
        addr, DEFAULT_CALDAV_DIRECTORY
    );
    println!();
    println!("To add a calendar event, use:");
    println!(
        "  curl -i -X PUT http://{}{}/my-calendar/event1.ics \\",
        addr, DEFAULT_CALDAV_DIRECTORY
    );
    println!("    -H 'Content-Type: text/calendar' \\");
    println!("    --data-binary @event.ics");
    println!();
    println!("Example event.ics content:");
    println!("BEGIN:VCALENDAR");
    println!("VERSION:2.0");
    println!("PRODID:-//Example Corp//CalDAV Client//EN");
    println!("BEGIN:VEVENT");
    println!("UID:12345@example.com");
    let next_year = chrono::Local::now().year_ce().1 + 1;
    println!("DTSTART:{}0101T120000Z", next_year);
    println!("DTEND:{}0101T130000Z", next_year);
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
                            async move {
                                // Handle .well-known/caldav redirect
                                if req.uri().path() == "/.well-known/caldav" {
                                    let mut response = hyper::Response::new(Body::empty());
                                    *response.status_mut() = hyper::StatusCode::MOVED_PERMANENTLY;
                                    response.headers_mut().insert(
                                        hyper::header::LOCATION,
                                        HeaderValue::from_static(DEFAULT_CALDAV_DIRECTORY),
                                    );
                                    return Ok::<_, Infallible>(response);
                                }
                                Ok::<_, Infallible>(dav_server.handle(req).await)
                            }
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
