//! ## Generic async HTTP/Webdav handler
//!
//! [`Webdav`] (RFC4918) is defined as
//! HTTP (GET/HEAD/PUT/DELETE) plus a bunch of extension methods (PROPFIND, etc).
//! These extension methods are used to manage collections (like unix directories),
//! get information on collections (like unix `ls` or `readdir`), rename and
//! copy items, lock/unlock items, etc.
//!
//! A `handler` is a piece of code that takes a `http::Request`, processes it in some
//! way, and then generates a `http::Response`. This library is a `handler` that maps
//! the HTTP/Webdav protocol to the filesystem. Or actually, "a" filesystem. Included
//! is an adapter for the local filesystem (`localfs`), and an adapter for an
//! in-memory filesystem (`memfs`).
//!
//! So this library can be used as a handler with HTTP servers like [hyper],
//! [warp], [actix-web], etc. Either as a correct and complete HTTP handler for
//! files (GET/HEAD) or as a handler for the entire Webdav protocol. In the latter case, you can
//! mount it as a remote filesystem: Linux, Windows, macOS can all mount Webdav filesystems.
//!
//! ## Backend interfaces.
//!
//! The backend interfaces are similar to the ones from the Go `x/net/webdav package`:
//!
//! - the library contains a [HTTP handler][DavHandler].
//! - you supply a [filesystem][DavFileSystem] for backend storage, which can optionally
//!   implement reading/writing [DAV properties][DavProp]. If the file system requires
//!   authorization, implement a [special trait][GuardedFileSystem].
//! - you can supply a [locksystem][DavLockSystem] that handles webdav locks.
//!
//! The handler in this library works with the standard http types
//! from the `http` and `http_body` crates. That means that you can use it
//! straight away with http libraries / frameworks that also work with
//! those types, like hyper. Compatibility modules for [actix-web][actix-compat]
//! and [warp][warp-compat] are also provided.
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
//! Also implemented is `partial PUT`, for which there are currently two
//! non-standard ways to do it: [`PUT` with the `Content-Range` header][PUT],
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
//! You're able to implement custom filesystem adapter:
//!
//! - [`DavFileSystem`]: without authorization.
//! - [`GuardedFileSystem`]: when access control is required.
//!
//! Also included are two locksystems:
//!
//! - [`MemLs`]: ephemeral in-memory locksystem.
//! - [`FakeLs`]: fake locksystem. just enough LOCK/UNLOCK support for macOS/Windows.
//!
//! External filesystem adapter implementations:
//!
//! - [`OpendalFs`](https://github.com/apache/opendal/tree/main/integrations/dav-server):
//!   connects various storage protocols via [OpenDAL](https://github.com/apache/opendal).
//!
//! ## Example.
//!
//! Example server using [hyper] that serves the /tmp directory in r/w mode. You should be
//! able to mount this network share from Linux, macOS and Windows. [Examples][examples]
//! for other frameworks are also available.
//!
//! ```no_run
//! use std::{convert::Infallible, net::SocketAddr};
//! use hyper::{server::conn::http1, service::service_fn};
//! use hyper_util::rt::TokioIo;
//! use tokio::net::TcpListener;
//! use dav_server::{fakels::FakeLs, localfs::LocalFs, DavHandler};
//!
//! #[tokio::main]
//! async fn main() {
//!     let dir = "/tmp";
//!     let addr: SocketAddr = ([127, 0, 0, 1], 4918).into();
//!
//!     let dav_server = DavHandler::builder()
//!         .filesystem(LocalFs::new(dir, false, false, false))
//!         .locksystem(FakeLs::new())
//!         .build_handler();
//!
//!     let listener = TcpListener::bind(addr).await.unwrap();
//!
//!     println!("Listening {addr}");
//!
//!     // We start a loop to continuously accept incoming connections
//!     loop {
//!         let (stream, _) = listener.accept().await.unwrap();
//!         let dav_server = dav_server.clone();
//!
//!         // Use an adapter to access something implementing `tokio::io` traits as if they implement
//!         // `hyper::rt` IO traits.
//!         let io = TokioIo::new(stream);
//!
//!         // Spawn a tokio task to serve multiple connections concurrently
//!         tokio::task::spawn(async move {
//!             // Finally, we bind the incoming connection to our `hello` service
//!             if let Err(err) = http1::Builder::new()
//!                 // `service_fn` converts our function in a `Service`
//!                 .serve_connection(
//!                     io,
//!                     service_fn({
//!                         move |req| {
//!                             let dav_server = dav_server.clone();
//!                             async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
//!                         }
//!                     }),
//!                 )
//!                 .await
//!             {
//!                 eprintln!("Failed serving: {err:?}");
//!             }
//!         });
//!     }
//! }
//! ```
//! [DavHandler]: struct.DavHandler.html
//! [DavFileSystem]: fs/index.html
//! [DavLockSystem]: ls/index.html
//! [DavProp]: fs/struct.DavProp.html
//! [`WebDav`]: https://tools.ietf.org/html/rfc4918
//! [RFC4918]: https://tools.ietf.org/html/rfc4918
//! [`MemLs`]: memls/index.html
//! [`MemFs`]: memfs/index.html
//! [`LocalFs`]: localfs/index.html
//! [`FakeLs`]: fakels/index.html
//! [actix-compat]: actix/index.html
//! [warp-compat]: warp/index.html
//! [README_litmus]: https://github.com/messense/dav-server-rs/blob/main/README.litmus-test.md
//! [examples]: https://github.com/messense/dav-server-rs/tree/main/examples/
//! [PUT]: https://github.com/messense/dav-server-rs/tree/main/doc/Apache-PUT-with-Content-Range.md
//! [PATCH]: https://github.com/messense/dav-server-rs/tree/main/doc/SABREDAV-partialupdate.md
//! [hyper]: https://hyper.rs/
//! [warp]: https://crates.io/crates/warp
//! [actix-web]: https://actix.rs/

#![cfg_attr(docsrs, feature(doc_cfg))]

#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;

mod async_stream;
mod conditional;
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
#[cfg(any(docsrs, feature = "localfs"))]
#[cfg_attr(docsrs, doc(cfg(feature = "localfs")))]
mod localfs_macos;
#[cfg(any(docsrs, feature = "localfs"))]
#[cfg_attr(docsrs, doc(cfg(feature = "localfs")))]
mod localfs_windows;
mod multierror;
mod tree;
mod util;
mod voidfs;
mod xmltree_ext;

pub mod body;
pub mod davpath;
pub mod fakels;
pub mod fs;
#[cfg(any(docsrs, feature = "localfs"))]
#[cfg_attr(docsrs, doc(cfg(feature = "localfs")))]
pub mod localfs;
pub mod ls;
#[cfg(any(docsrs, feature = "memfs"))]
#[cfg_attr(docsrs, doc(cfg(feature = "memfs")))]
pub mod memfs;
pub mod memls;

#[cfg(any(docsrs, feature = "actix-compat"))]
#[cfg_attr(docsrs, doc(cfg(feature = "actix-compat")))]
pub mod actix;

#[cfg(any(docsrs, feature = "warp-compat"))]
#[cfg_attr(docsrs, doc(cfg(feature = "warp-compat")))]
pub mod warp;

pub(crate) use crate::davhandler::DavInner;
pub(crate) use crate::errors::{DavError, DavResult};
pub(crate) use crate::fs::*;

pub use crate::davhandler::{DavConfig, DavHandler};
pub use crate::util::{DavMethod, DavMethodSet};
