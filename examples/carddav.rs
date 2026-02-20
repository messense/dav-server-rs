//! CardDAV server example
//!
//! This example demonstrates how to set up a CardDAV server using the dav-server library.
//! CardDAV is an extension of WebDAV for contact/address book data management.
//!
//! Usage:
//!   cargo run --example carddav --features carddav
//!
//! The server will be available at http://localhost:8080
//! You can connect to it using CardDAV clients like Thunderbird, Apple Contacts, etc.

use axum::{
    Extension, Router,
    body::Body,
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::{self, Next},
    response::IntoResponse,
    routing::any,
};
use dav_server::{DavHandler, carddav::DEFAULT_CARDDAV_DIRECTORY, fakels::FakeLs, localfs};
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
        // For a real world application you would have your own GuardedFilesystem
        // and use server.handle_guarded(req, format!("/principals/users/{user_name}"), credentials)
        .principal("/addressbooks")
        .build_handler();

    let router = Router::new()
        .route("/.well-known/carddav", any(handle_carddav_redirect))
        .route("/", any(handle_carddav))
        .route("/{*path}", any(handle_carddav))
        .layer(Extension(Arc::new(dav_server)))
        .layer(middleware::from_fn(log_request_middleware));

    let listener = TcpListener::bind(&addr).await.unwrap();

    println!("CardDAV server listening on http://{}", addr);
    println!(
        "Address book collections can be accessed at http://{}{}",
        addr, DEFAULT_CARDDAV_DIRECTORY
    );
    println!();
    println!(
        "NOTE: This example stores data in a temporary directory (/tmp). Data may be lost when the server stops or when temporary files are cleaned."
    );
    println!();
    println!("To create an address book collection, use:");
    println!(
        "  curl -i -X MKADDRESSBOOK http://{}{}/my-contacts/",
        addr, DEFAULT_CARDDAV_DIRECTORY
    );
    println!();
    println!("To add a contact, use:");
    println!(
        "  curl -i -X PUT http://{}{}/my-contacts/contact1.vcf \\",
        addr, DEFAULT_CARDDAV_DIRECTORY
    );
    println!("    -H 'Content-Type: text/vcard' \\");
    println!("    --data-binary @contact.vcf");
    println!();
    println!("Example contact.vcf content:");
    println!("BEGIN:VCARD");
    println!("VERSION:3.0");
    println!("UID:12345@example.com");
    println!("FN:John Doe");
    println!("N:Doe;John;;;");
    println!("EMAIL:john.doe@example.com");
    println!("TEL:+1-555-123-4567");
    println!("END:VCARD");

    axum::serve(listener, router).await.unwrap();
}

async fn handle_carddav_redirect() -> (
    StatusCode,
    [(axum::http::header::HeaderName, HeaderValue); 1],
) {
    (
        StatusCode::MOVED_PERMANENTLY,
        [(
            axum::http::header::LOCATION,
            HeaderValue::from_static(DEFAULT_CARDDAV_DIRECTORY),
        )],
    )
}

async fn handle_carddav(
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

#[cfg(not(feature = "carddav"))]
fn main() {
    eprintln!("This example requires the 'carddav' feature to be enabled.");
    eprintln!("Run with: cargo run --example carddav --features carddav");
    std::process::exit(1);
}
