#![doc(html_root_url = "https://docs.rs/webdav-handler/0.1.0")]
//! ## Generic async HTTP/WEBDAV handler
//!
//! [`Webdav`] ([RFC4918]) is HTTP (GET/HEAD/PUT/DELETE) plus a bunch of extra methods.
//!
//! This crate implements a futures/stream based webdav handler for Rust, using
//! the types from the `http` crate. It comes complete with a async filesystem
//! backend, so it can be used as a HTTP or WEBDAV fileserver.
//!
//! NOTE: this crate uses futures 0.3 + async/await code internally, so it
//! only works on Rust nightly (currently rustc 1.35.0-nightly (4c27fb19b 2019-03-25)).
//! The external interface is futures 0.1 based though (might add 0.3 as well).
//!
//! ## Interface.
//!
//! It has an interface similar to the Go x/net/webdav package:
//!
//! - the library contains an [HTTP handler](DavHandler)
//! - you supply a [filesystem](DavFileSystem) for backend storage, which can optionally
//!   implement reading/writing [DAV properties](DavProp).
//! - you can supply a [locksystem][DavLockSystem] that handles the webdav locks
//!
//! With some glue code, this handler can be used from HTTP server
//! libraries/frameworks such as [hyper] or [actix-web].
//! (See [examples/hyper.rs][hyper_example] or [examples/actix-web][actix_web_example]).
//!
//! ## Implemented standards.
//!
//! Currently [passes the "basic", "copymove", "props", "locks" and "http"
//! checks][README_litmus] of the Webdav Litmus Test testsuite. That's all of the base
//! [RFC4918] webdav specification.
//!
//! The litmus test suite also has tests for RFC3744 "acl" and "principal",
//! RFC5842 "bind", and RFC3253 "versioning". Those we do not support right now.
//!
//! The relevant parts of the HTTP RFCs are also implemented, such as the
//! preconditions (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since,
//! If-Range), partial transfers (Range).
//!
//! Also implemented is `partial PUT`, for which there are currently [two
//! non-standard ways][PartialPut] to do it: [`PUT` with the `Content-Range` header][PUT],
//! which is what Apache's `mod_dav` implements, and [`PATCH` with the `X-Update-Range`
//! header][PATCH] from `SabreDav`.
//!
//! ## Backends.
//!
//! Included are two filesystems:
//!
//! - [`LocalFs`]: serves a directory on the local filesystem
//! - [`MemFs`]: ephemeral in-memory filesystem. supports DAV properties.
//!
//! Also included are two locksystems:
//!
//! - [`MemLs`]: ephemeral in-memory locksystem.
//! - [`FakeLs`]: fake locksystem. just enough LOCK/UNLOCK support for OSX/Windows.
//!
//! ## Example.
//!
//! Example server that serves the /tmp directory in r/w mode. You should be
//! able to mount this network share from Linux, OSX and Windows.
//!
//! ```no_run
//! # extern crate futures01 as futures;
//! use hyper;
//! use bytes::Bytes;
//! use futures::{future::Future, stream::Stream};
//! use webdav_handler::{DavHandler, localfs::LocalFs, fakels::FakeLs};
//!
//! fn main() {
//!     let dir = "/tmp";
//!     let addr = ([127, 0, 0, 1], 4918).into();
//!
//!     let dav_server = DavHandler::new(None, LocalFs::new(dir, false, false, false), Some(FakeLs::new()));
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
//! [DavHandler]: struct.DavHandler.html
//! [DavFileSystem]: fs/struct.DavFileSystem.html
//! [DavLockSystem]: ls/struct.DavLockSystem.html
//! [DavProp]: fs/struct.DavProp.html
//! [`WebDav`]: https://tools.ietf.org/html/rfc4918
//! [RFC4918]: https://tools.ietf.org/html/rfc4918
//! [`MemLs`]: memls/index.html
//! [`MemFs`]: memfs/index.html
//! [`LocalFs`]: localfs/index.html
//! [`FakeLs`]: fakels/index.html
//! [README_litmus]: https://github.xs4all.net/mikevs/webdav-handler-rs/blob/master/README.litmus-test.md
//! [hyper_example]: https://github.xs4all.net/mikevs/webdav-handler-rs/blob/master/examples/hyper.rs
//! [actix_web_example]: https://github.xs4all.net/mikevs/webdav-handler-rs/blob/master/examples/actix-web.rs
//! [PartialPut]: https://blog.sphere.chronosempire.org.uk/2012/11/21/webdav-and-the-http-patch-nightmare
//! [PUT]: https://blog.sphere.chronosempire.org.uk/2012/11/21/webdav-and-the-http-patch-nightmare
//! [PATCH]: https://github.com/miquels/webdavfs/blob/master/SABREDAV-partialupdate.md
//! [hyper]: https://hyper.rs/
//! [actix-web]: https://actix.rs/
//!
#![feature(async_await, await_macro)]

#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;

mod conditional;
pub mod corostream;
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

pub mod fakels;
pub mod fs;
pub mod localfs;
mod localfs_macos;
mod localfs_windows;
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
