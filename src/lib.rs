//! `Webdav` (RFC4918) is HTTP (GET/HEAD/PUT/DELETE) plus a bunch of extra methods.
//!
//! This crate implements a futures/stream based webdav handler for Rust, using
//! the types from the `http` crate. It comes complete with a async filesystem
//! backend, so it can be used as a HTTP or WEBDAV fileserver.
//!
//! NOTE: this crate uses futures 0.3 + async/await code internally, so it
//! only works on Rust nightly (currently rustc 1.34.0-nightly (00aae71f5 2019-02-25)).
//! The external interface is futures 0.1 based though, so it can work with
//! stable hyper and actix.
//!
//! It has an interface similar to the Go x/net/webdav package:
//!
//! - the library contains an HTTP handler
//! - you supply a "filesystem" for backend storage, which can optionally
//!   implement reading/writing "DAV properties"
//! - you can supply a "locksystem" that handles the webdav locks
//!
//! With some glue code, this handler can be used from HTTP server
//! libraries/frameworks such as hyper or actix-web.
//!
//! Currently passes the "basic", "copymove", "props", "locks" and "http"
//! checks of the Webdav Litmus Test testsuite. That's all of the base
//! RFC4918 webdav specification.
//!
//! The litmus test suite also has tests for RFC3744 "acl" and "principal",
//! RFC5842 "bind", and RFC3253 "versioning". Those we do not support right now.
//!
//! The relevant parts of the HTTP RFCs are also implemented, such as the
//! preconditions (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since,
//! If-Range), partial transfers (Range).
//!
//! Also implemented is partial PUT, for which there are currently two
//! non-standard ways to do it: `PUT` with the `Content-Range` header, which is what
//! Apache's `mod_dav` implements, and `PATCH` with the `X-Update-Range` header
//! from `SabreDav`.
//!
//! Included are two filesystems:
//!
//! - localfs: serves a directory on the local filesystem
//! - memfs: ephemeral in-memory filesystem. supports DAV properties.
//!
//! Also included are two locksystems:
//!
//! - memls: ephemeral in-memory locksystem.
//! - fakels: fake locksystem. just enough LOCK/UNLOCK support for OSX/Windows.
//!
//! Example server that serves the /tmp directory in r/w mode. You should be
//! able to mount this network share from Linux, OSX and Windows.
//!
//! ```no_run
//! use hyper;
//! use bytes::Bytes;
//! use futures::{future::Future, stream::Stream};
//! use webdav_handler::{DavHandler, localfs::LocalFs, fakels::FakeLs};
//!
//! fn main() {
//!     let dir = "/tmp";
//!     let addr = ([127, 0, 0, 1], 4918).into();
//!
//!     let dav_server = DavHandler::new(None, LocalFs::new(dir, false), Some(FakeLs::new()));
//!     let make_service = move || {
//!         let dav_server = dav_server.clone();
//!         hyper::service::service_fn(move |req: hyper::Request<hyper::Body>| {
//!             /// Turn hyper request body stream into more general Bytes stream.
//!             let (parts, body) = req.into_parts();
//!             let body = body.map(|item| Bytes::from(item));
//!             let req = http::Request::from_parts(parts, body);
//!             let fut = dav_server.handle(req)
//!                 .and_then(|resp| {
//!                     /// Transform the response Byte stream into a hyper response body.
//!                     let (parts, body) = resp.into_parts();
//!                     let body = hyper::Body::wrap_stream(body);
//!                     Ok(hyper::Response::from_parts(parts, body))
//!                 });
//!             Box::new(fut)
//!         })
//!     };
//!
//!     println!("Serving {} on {}", dir, addr);
//!     let server = hyper::Server::bind(&addr)
//!         .serve(make_service)
//!         .map_err(|e| eprintln!("server error: {}", e));
//!
//!     hyper::rt::run(server);
//! }
//! ```
#![feature(async_await, await_macro, futures_api)]

#[macro_use]
extern crate hyperx;
#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;
#[macro_use]
extern crate percent_encoding;

mod conditional;
mod corostream;
mod davhandler;
mod davheaders;
mod errors;
mod handle_copymove;
mod handle_delete;
mod handle_gethead;
mod handle_lock;
mod handle_mkcol;
mod handle_options;
mod handle_props;
mod handle_put;
mod multierror;
mod tree;
mod util;
mod xmltree_ext;

#[doc(hidden)]
pub mod typed_headers;

pub mod fakels;
pub mod fs;
pub mod localfs;
pub mod ls;
pub mod memfs;
pub mod memls;
pub mod webpath;

use std::io;

use bytes::Bytes;

pub(crate) use crate::davhandler::DavInner;
pub(crate) use crate::errors::{DavError, DavResult};
pub(crate) use crate::fs::*;

/// A boxed futures 0.1 Stream of Bytes.
#[allow(unused)]
pub type BoxedByteStream = Box<futures01::Stream<Item = Bytes, Error = io::Error> + Send + 'static>;

pub use crate::davhandler::{DavConfig, DavHandler};
pub use crate::util::{AllowedMethods, Method};
