//!
//! A futures/stream based webdav handler for Rust, using the types from
//! the `http` crate. It has an interface similar to the Go x/net/webdav package:
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
//!             let (parts, body) = req.into_parts();
//!             let body = body.map(|item| Bytes::from(item));
//!             let req = http::Request::from_parts(parts, body);
//!             let fut = dav_server.handle(req)
//!                 .and_then(|resp| {
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

mod common;
mod conditional;
mod corostream;
mod errors;
mod handle_copymove;
mod handle_delete;
mod handle_gethead;
mod handle_lock;
mod handle_mkcol;
mod handle_options;
mod handle_props;
mod handle_put;
mod headers;
mod multierror;
mod tree;
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

use std::error::Error as StdError;
use std::io;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{self, Bytes};

use futures::future;
use futures::prelude::*;
use futures03::future::{FutureExt, TryFutureExt};
use futures03::stream::{StreamExt, TryStreamExt};

use http::Method as httpMethod;
use http::{Request, Response, StatusCode};

use crate::typed_headers::HeaderMapExt;
use crate::webpath::WebPath;

pub(crate) use crate::errors::DavError;
pub(crate) use crate::fs::*;
pub(crate) use crate::ls::*;

#[allow(unused)]
pub type BoxedByteStream = Box<Stream<Item = Bytes, Error = io::Error> + Send + 'static>;
pub(crate) type DavResult<T> = Result<T, DavError>;
pub(crate) type BytesResult = io::Result<Bytes>;

/// HTTP Methods supported by DavHandler.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(u32)]
pub enum Method {
    Head      = 0x0001,
    Get       = 0x0002,
    Put       = 0x0004,
    Patch     = 0x0008,
    Options   = 0x0010,
    PropFind  = 0x0020,
    PropPatch = 0x0040,
    MkCol     = 0x0080,
    Copy      = 0x0100,
    Move      = 0x0200,
    Delete    = 0x0400,
    Lock      = 0x0800,
    Unlock    = 0x1000,
}

/// The webdav handler struct.
#[derive(Clone)]
pub struct DavHandler {
    config: Arc<DavConfig>,
}

/// Configuration of the handler.
#[derive(Default)]
pub struct DavConfig {
    /// Prefix to be stripped off when handling request.
    pub prefix: Option<String>,
    /// Filesystem backend.
    pub fs: Option<Box<DavFileSystem>>,
    /// Locksystem backend.
    pub ls: Option<Box<DavLockSystem>>,
    /// Set of allowed methods (None means "all methods")
    pub allow: Option<AllowedMethods>,
    /// Principal is webdav speak for "user", used to give locks an owner (if a locksystem is
    /// active).
    pub principal: Option<String>,
}

// The actual inner struct.
pub(crate) struct DavInner {
    pub prefix:    String,
    pub fs:        Box<DavFileSystem>,
    pub ls:        Option<Box<DavLockSystem>>,
    pub allow:     Option<AllowedMethods>,
    pub principal: Option<String>,
}

impl From<DavConfig> for DavInner {
    fn from(cfg: DavConfig) -> Self {
        DavInner {
            prefix:    cfg.prefix.unwrap_or("".to_string()),
            fs:        cfg.fs.unwrap(),
            ls:        cfg.ls,
            allow:     cfg.allow,
            principal: cfg.principal,
        }
    }
}

impl From<&DavConfig> for DavInner {
    fn from(cfg: &DavConfig) -> Self {
        DavInner {
            prefix:    cfg
                .prefix
                .as_ref()
                .map(|p| p.to_owned())
                .unwrap_or("".to_string()),
            fs:        cfg.fs.clone().unwrap(),
            ls:        cfg.ls.clone(),
            allow:     cfg.allow,
            principal: cfg.principal.clone(),
        }
    }
}

impl Clone for DavInner {
    fn clone(&self) -> Self {
        DavInner {
            prefix:    self.prefix.clone(),
            fs:        self.fs.clone(),
            ls:        self.ls.clone(),
            allow:     self.allow.clone(),
            principal: self.principal.clone(),
        }
    }
}

pub(crate) fn systemtime_to_timespec(t: SystemTime) -> time::Timespec {
    match t.duration_since(UNIX_EPOCH) {
        Ok(t) => {
            time::Timespec {
                sec:  t.as_secs() as i64,
                nsec: 0,
            }
        },
        Err(_) => time::Timespec { sec: 0, nsec: 0 },
    }
}

pub(crate) fn systemtime_to_httpdate(t: SystemTime) -> typed_headers::HttpDate {
    typed_headers::HttpDate::from(t)
}

pub(crate) fn systemtime_to_rfc3339(t: SystemTime) -> String {
    let ts = systemtime_to_timespec(t);
    format!("{}", time::at_utc(ts).rfc3339())
}

// translate method into our own enum that has webdav methods as well.
pub(crate) fn dav_method(m: &http::Method) -> DavResult<Method> {
    let m = match m {
        &httpMethod::HEAD => Method::Head,
        &httpMethod::GET => Method::Get,
        &httpMethod::PUT => Method::Put,
        &httpMethod::PATCH => Method::Patch,
        &httpMethod::DELETE => Method::Delete,
        &httpMethod::OPTIONS => Method::Options,
        _ => {
            match m.as_str() {
                "PROPFIND" => Method::PropFind,
                "PROPPATCH" => Method::PropPatch,
                "MKCOL" => Method::MkCol,
                "COPY" => Method::Copy,
                "MOVE" => Method::Move,
                "LOCK" => Method::Lock,
                "UNLOCK" => Method::Unlock,
                _ => {
                    return Err(DavError::UnknownMethod);
                },
            }
        },
    };
    Ok(m)
}

// helper.
pub(crate) fn empty_body() -> BoxedByteStream {
    Box::new(futures03::stream::empty::<BytesResult>().compat())
}

pub(crate) fn single_body(body: impl Into<Bytes>) -> BoxedByteStream {
    let body = vec![Ok::<Bytes, io::Error>(body.into())].into_iter();
    Box::new(futures03::stream::iter(body).compat())
}

/// A set of allowed `Method`s.
#[derive(Clone, Copy)]
pub struct AllowedMethods(u32);

impl AllowedMethods {
    /// New set, all methods allowed.
    pub fn all() -> AllowedMethods {
        AllowedMethods(0xffffffff)
    }

    /// New set, no methods allowed.
    pub fn none() -> AllowedMethods {
        AllowedMethods(0)
    }

    /// Add a method.
    pub fn add(&mut self, m: Method) -> &Self {
        self.0 |= m as u32;
        self
    }

    /// Remove a method.
    pub fn remove(&mut self, m: Method) -> &Self {
        self.0 &= !(m as u32);
        self
    }

    /// Check if method is allowed.
    pub fn allowed(&self, m: Method) -> bool {
        self.0 & (m as u32) > 0
    }
}

// return a 404 reply.
fn notfound() -> impl Future<Item = http::Response<BoxedByteStream>, Error = io::Error> {
    let body = futures::stream::once(Ok(bytes::Bytes::from("Not Found")));
    let body: BoxedByteStream = Box::new(body);
    let response = http::Response::builder()
        .status(404)
        .header("connection", "close")
        .body(body)
        .unwrap();
    return Box::new(futures::future::ok(response));
}

impl DavHandler {
    /// Create a new `DavHandler`.
    /// - `prefix`: URL prefix to be stripped off.
    /// - `fs:` The filesystem backend.
    /// - `ls:` Optional locksystem backend
    pub fn new(prefix: Option<&str>, fs: Box<DavFileSystem>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        let config = DavConfig {
            prefix:    prefix.map(|s| s.to_string()),
            fs:        Some(fs),
            ls:        ls,
            allow:     None,
            principal: None,
        };
        DavHandler {
            config: Arc::new(config),
        }
    }

    /// Create a new `DavHandler` with a more detailed configuration.
    ///
    /// For example, pass in a specific `AllowedMethods` set.
    pub fn new_with(config: DavConfig) -> DavHandler {
        DavHandler {
            config: Arc::new(config),
        }
    }

    /// Handle a webdav request.
    ///
    /// Only one error kind is ever returned: `ErrorKind::BrokenPipe`. In that case we
    /// were not able to generate a response at all, and the server should just
    /// close the connection.
    pub fn handle<ReqBody, ReqError>(
        &self,
        req: Request<ReqBody>,
    ) -> impl Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        if self.config.fs.is_none() {
            return future::Either::A(notfound());
        }
        let inner = DavInner::from(&*self.config);
        future::Either::B(inner.handle(req))
    }

    /// Handle a webdav request, overriding parts of the config.
    ///
    /// For example, the `principal` can be set for this request.
    ///
    /// Or, the default config has no locksystem, and you pass in
    /// a fake locksystem (`FakeLs`) because this is a request from a
    /// windows or osx client that needs to see locking support.
    pub fn handle_with<ReqBody, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> impl Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let orig = &*self.config;
        let newconf = DavConfig {
            prefix:    config.prefix.or(orig.prefix.clone()),
            fs:        config.fs.or(orig.fs.clone()),
            ls:        config.ls.or(orig.ls.clone()),
            allow:     config.allow.or(orig.allow.clone()),
            principal: config.principal.or(orig.principal.clone()),
        };
        if newconf.fs.is_none() {
            return future::Either::A(notfound());
        }
        let inner = DavInner::from(newconf);
        future::Either::B(inner.handle(req))
    }
}

impl DavInner {
    // helper.
    pub(crate) async fn has_parent<'a>(&'a self, path: &'a WebPath) -> bool {
        let p = path.parent();
        await!(self.fs.metadata(&p))
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    // helper.
    pub(crate) fn path(&self, req: &Request<()>) -> WebPath {
        // This never fails (has been checked before)
        WebPath::from_uri(req.uri(), &self.prefix).unwrap()
    }

    // See if this is a directory and if so, if we have
    // to fixup the path by adding a slash at the end.
    pub(crate) fn fixpath(
        &self,
        res: &mut Response<BoxedByteStream>,
        path: &mut WebPath,
        meta: Box<DavMetaData>,
    ) -> Box<DavMetaData>
    {
        if meta.is_dir() && !path.is_collection() {
            path.add_slash();
            let newloc = path.as_url_string_with_prefix();
            res.headers_mut().typed_insert(headers::ContentLocation(newloc));
        }
        meta
    }

    // drain request body and return length.
    pub(crate) async fn read_request<ReqBody, ReqError>(
        &self,
        body: ReqBody,
        max_size: usize,
    ) -> DavResult<Vec<u8>>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let mut body = futures03::compat::Compat01As03::new(body);
        let mut data = Vec::new();
        while let Some(res) = await!(body.next()) {
            let chunk = res.map_err(|_| {
                DavError::IoError(io::Error::new(io::ErrorKind::UnexpectedEof, "UnexpectedEof"))
            })?;
            if data.len() + chunk.len() > max_size {
                return Err(StatusCode::PAYLOAD_TOO_LARGE.into());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    // internal dispatcher.
    fn handle<ReqBody, ReqError>(
        self,
        req: Request<ReqBody>,
    ) -> impl Future<Item = Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let fut = async move {

            // debug when running the webdav litmus tests.
            if log_enabled!(log::Level::Debug) {
                if let Some(t) = req.headers().typed_get::<headers::XLitmus>() {
                    debug!("X-Litmus: {}", t);
                }
            }

            // translate HTTP method to Webdav method.
            let method = match dav_method(req.method()) {
                Ok(m) => m,
                Err(e) => {
                    debug!("refusing method {} request {}", req.method(), req.uri());
                    return Err(e);
                },
            };

            // see if method is allowed.
            if let Some(ref a) = self.allow {
                if !a.allowed(method) {
                    debug!("method {} not allowed on request {}", req.method(), req.uri());
                    return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
                }
            }

            // make sure the request path is valid.
            let path = WebPath::from_uri(req.uri(), &self.prefix)?;

            let (req, body) = {
                let (parts, body) = req.into_parts();
                (Request::from_parts(parts, ()), body)
            };

            // PUT is the only handler that reads the body itself. All the
            // other handlers either expected no body, or a pre-read Vec<u8>.
            let (body_strm, body_data) = if method == Method::Put {
                (Some(body), Vec::new())
            } else {
                (None, await!(self.read_request(body, 65536))?)
            };

            // Not all methods accept a body.
            match method {
                Method::Put | Method::Patch | Method::PropFind | Method::PropPatch | Method::Lock => {},
                _ => {
                    if body_data.len() > 0 {
                        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE.into());
                    }
                },
            }

            debug!("== START REQUEST {:?} {}", method, path);

            let res = match method {
                Method::Options => await!(self.handle_options(req)),
                Method::PropFind => await!(self.handle_propfind(req, body_data)),
                Method::PropPatch => await!(self.handle_proppatch(req, body_data)),
                Method::MkCol => await!(self.handle_mkcol(req)),
                Method::Delete => await!(self.handle_delete(req)),
                Method::Lock => await!(self.handle_lock(req, body_data)),
                Method::Unlock => await!(self.handle_unlock(req)),
                Method::Head | Method::Get => await!(self.handle_get(req)),
                Method::Put | Method::Patch => await!(self.handle_put(req, body_strm.unwrap())),
                Method::Copy | Method::Move => await!(self.handle_copymove(req, method)),
            };
            res
        };

        // Turn any DavError results into a HTTP error response.
        async {
            match await!(fut) {
                Ok(resp) => {
                    debug!("== END REQUEST result OK");
                    Ok(resp)
                },
                Err(err) => {
                    debug!("== END REQUEST result {:?}", err);
                    let mut resp = Response::builder();
                    resp.status(err.statuscode());
                    if err.must_close() {
                        resp.header("connection", "close");
                    }
                    let resp = resp.body(empty_body()).unwrap();
                    Ok(resp)
                },
            }
        }
            .boxed()
            .compat()
    }
}
