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

use axum::{
    Extension, Router,
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::any,
};
use chrono::Datelike;
use dav_server::{DavHandler, caldav::DEFAULT_CALDAV_DIRECTORY, fakels::FakeLs, localfs};
use http_body_util::BodyExt;
use std::sync::Arc;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() {
    env_logger::init();
    let addr = "127.0.0.1:8080";

    let dav_server = DavHandler::builder()
        .filesystem(localfs::LocalFs::new("/tmp", true, false, false))
        .locksystem(FakeLs::new())
        .autoindex(true)
        .build_handler();

    let router = Router::new()
        .route("/.well-known/caldav", any(handle_caldav_redirect))
        .route("/", any(handle_caldav))
        .route("/{*path}", any(handle_caldav))
        .layer(Extension(Arc::new(dav_server)))
        .layer(middleware::from_fn(log_request_middleware));

    let listener = TcpListener::bind(&addr).await.unwrap();

    println!("CalDAV server listening on http://{}", addr);
    println!(
        "Calendar collections can be accessed at http://{}{}",
        addr, DEFAULT_CALDAV_DIRECTORY
    );
    println!();
    println!(
        "NOTE: This example stores data in a temporary directory (/tmp). Data may be lost when the server stops or when temporary files are cleaned."
    );
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

    axum::serve(listener, router).await.unwrap();
}

async fn handle_caldav_redirect() -> (
    StatusCode,
    [(axum::http::header::HeaderName, HeaderValue); 1],
) {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            axum::http::header::LOCATION,
            HeaderValue::from_static(DEFAULT_CALDAV_DIRECTORY),
        )],
    )
}

async fn handle_caldav(
    Extension(dav): Extension<Arc<DavHandler>>,
    req: Request,
) -> impl IntoResponse {
    dav.handle(req).await
}

async fn log_request_middleware(request: Request, next: Next) -> impl IntoResponse {
    // Print request line and headers
    println!("\n========== CLIENT REQUEST ==========");
    println!("{} {}", request.method(), request.uri(),);
    println!("--- Headers ---");
    for (name, value) in request.headers() {
        println!("{}: {}", name, value.to_str().unwrap_or("<binary>"));
    }

    // Read and print body
    let (parts, body) = request.into_parts();
    let collected = body.collect().await.unwrap_or_default();
    let body_bytes = collected.to_bytes();

    if !body_bytes.is_empty() {
        println!("--- Body ---");
        if let Ok(body_str) = std::str::from_utf8(&body_bytes) {
            println!("{}", body_str);
        } else {
            println!("<binary data: {} bytes>", body_bytes.len());
        }
    }
    println!("====================================\n");

    // Reconstruct request with body
    let request = axum::http::Request::from_parts(parts, Body::from(body_bytes));

    next.run(request).await
}

#[cfg(not(feature = "caldav"))]
fn main() {
    eprintln!("This example requires the 'caldav' feature to be enabled.");
    eprintln!("Run with: cargo run --example caldav --features caldav");
    std::process::exit(1);
}
