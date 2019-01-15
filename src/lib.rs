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

#[macro_use] extern crate hyperx;
#[macro_use] extern crate log;
#[macro_use] extern crate lazy_static;
#[macro_use] extern crate percent_encoding;

mod errors;
mod headers;
mod handle_copymove;
mod handle_delete;
mod handle_gethead;
mod handle_lock;
mod handle_mkcol;
mod handle_options;
mod handle_props;
mod handle_put;
mod multierror;
mod conditional;
mod xmltree_ext;
mod tree;

mod sync_adapter;

#[doc(hidden)]
pub mod typed_headers;

pub mod fs;
pub mod ls;
pub mod localfs;
pub mod memfs;
pub mod memls;
pub mod fakels;
pub mod webpath;

use std::io::Read;
use std::time::{UNIX_EPOCH,SystemTime};
use std::sync::Arc;

use bytes;
use futures::{future, Future, Stream};

use http::Method as httpMethod;
use http::StatusCode;

use crate::typed_headers::HeaderMapExt;
use crate::sync_adapter::{Request, Response};
use crate::webpath::WebPath;

pub(crate) use crate::errors::DavError;
pub(crate) use crate::fs::*;
pub(crate) use crate::ls::*;

pub use crate::sync_adapter::BoxedByteStream;

type DavResult<T> = Result<T, DavError>;

/// HTTP Methods supported by DavHandler.
#[derive(Debug,PartialEq,Eq,Hash,Clone,Copy)]
#[repr(u32)]
pub enum Method {
    Head        = 0x0001,
    Get         = 0x0002,
    Put         = 0x0004,
    Patch       = 0x0008,
    Options     = 0x0010,
    PropFind    = 0x0020,
    PropPatch   = 0x0040,
    MkCol       = 0x0080,
    Copy        = 0x0100,
    Move        = 0x0200,
    Delete      = 0x0400,
    Lock        = 0x0800,
    Unlock      = 0x1000,
}

/// The webdav handler struct.
#[derive(Clone)]
pub struct DavHandler {
    config:     Arc<DavConfig>,
}

/// Configuration of the handler.
#[derive(Default)]
pub struct DavConfig {
    /// Prefix to be stripped off when handling request.
    pub prefix:     Option<String>,
    /// Filesystem backend.
    pub fs:         Option<Box<DavFileSystem>>,
    /// Locksystem backend.
    pub ls:         Option<Box<DavLockSystem>>,
    /// Set of allowed methods (None means "all methods")
    pub allow:      Option<AllowedMethods>,
    /// Principal is webdav speak for "user", used to give locks an owner (if a locksystem is
    /// active).
    pub principal:  Option<String>,
    /// Closures to be called in the worker thread at the start and the end of the request.
    pub reqhooks:   Option<(Box<Fn() + Send + Sync + 'static>, Box<Fn() + Send + Sync + 'static>)>,
}

// The actual inner struct.
pub (crate) struct DavInner {
    pub prefix:     String,
    pub fs:         Box<DavFileSystem>,
    pub ls:         Option<Box<DavLockSystem>>,
    pub allow:      Option<AllowedMethods>,
    pub principal:  Option<String>,
    pub reqhooks:   Option<(Box<Fn() + Send + Sync + 'static>, Box<Fn() + Send + Sync + 'static>)>,
}

impl From<DavConfig> for DavInner {
    fn from(cfg: DavConfig) -> Self {
        DavInner {
            prefix:     cfg.prefix.unwrap_or("".to_string()),
            fs:         cfg.fs.unwrap(),
            ls:         cfg.ls,
            allow:      cfg.allow,
            principal:  cfg.principal,
            reqhooks:   cfg.reqhooks,
        }
    }
}

impl From<&DavConfig> for DavInner {
    fn from(cfg: &DavConfig) -> Self {
        DavInner {
            prefix:     cfg.prefix.as_ref().map(|p| p.to_owned()).unwrap_or("".to_string()),
            fs:         cfg.fs.clone().unwrap(),
            ls:         cfg.ls.clone(),
            allow:      cfg.allow,
            principal:  cfg.principal.clone(),
            reqhooks:    None,
        }
    }
}

pub(crate) fn systemtime_to_timespec(t: SystemTime) -> time::Timespec {
    match t.duration_since(UNIX_EPOCH) {
        Ok(t) => time::Timespec{
            sec: t.as_secs() as i64,
            nsec:0,
        },
        Err(_) => time::Timespec{sec: 0, nsec: 0},
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
        _ => match m.as_str() {
            "PROPFIND" => Method::PropFind,
            "PROPPATCH" => Method::PropPatch,
            "MKCOL" => Method::MkCol,
            "COPY" => Method::Copy,
            "MOVE" => Method::Move,
            "LOCK" => Method::Lock,
            "UNLOCK" => Method::Unlock,
            _ => {
                return Err(DavError::UnknownMethod);
            }
        }
    };
    Ok(m)
}

// map_err helper.
pub (crate) fn statuserror(res: &mut Response, s: StatusCode) -> DavError {
    *res.status_mut() = s;
    DavError::Status(s)
}

// map_err helper.
fn daverror<E: Into<DavError>>(res: &mut Response, e: E) -> DavError {
    let err = e.into();
    *res.status_mut() = err.statuscode();
    err
}

// map_err helper.
pub (crate) fn fserror(res: &mut Response, e: FsError) -> DavError {
    let s = fserror_to_status(e);
    *res.status_mut() = s;
    DavError::Status(s)
}

// helper.
pub (crate) fn fserror_to_status(e: FsError) -> StatusCode {
    match e {
        FsError::NotImplemented => StatusCode::NOT_IMPLEMENTED,
        FsError::GeneralFailure => StatusCode::INTERNAL_SERVER_ERROR,
        FsError::Exists => StatusCode::METHOD_NOT_ALLOWED,
        FsError::NotFound => StatusCode::NOT_FOUND,
        FsError::Forbidden => StatusCode::FORBIDDEN,
        FsError::InsufficientStorage => StatusCode::INSUFFICIENT_STORAGE,
        FsError::LoopDetected => StatusCode::LOOP_DETECTED,
        FsError::PathTooLong => StatusCode::URI_TOO_LONG,
        FsError::TooLarge => StatusCode::PAYLOAD_TOO_LARGE,
        FsError::IsRemote => StatusCode::BAD_GATEWAY,
    }
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
fn notfound() -> impl Future<Item = http::Response<BoxedByteStream>, Error = std::io::Error> {
	let body = futures::stream::once(Ok(bytes::Bytes::from("Not Found")));
	let body: BoxedByteStream = Box::new(body);
	let response = http::Response::builder()
		.status(404)
		.header("connection", "close")
		.body(body)
		.unwrap();
	return Box::new(futures::future::ok(response));
}

// helper to call a closure on drop.
struct Dropper(Option<Box<dyn Fn()>>);
impl Drop for Dropper {
    fn drop(&mut self) {
        if let Some(f) = &self.0 {
            f()
        }
    }
}

impl DavHandler {
    /// Create a new `DavHandler`.
    /// - `prefix`: URL prefix to be stripped off.
    /// - `fs:` The filesystem backend.
    /// - `ls:` Optional locksystem backend
    pub fn new(prefix: Option<&str>, fs: Box<DavFileSystem>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        let config = DavConfig{
            prefix: prefix.map(|s| s.to_string()),
            fs: Some(fs),
            ls: ls,
            allow: None,
            principal: None,
            reqhooks: None,
        };
        DavHandler{ config: Arc::new(config) }
    }

    /// Create a new `DavHandler` with a more detailed configuration.
    ///
    /// For example, pass in a specific `AllowedMethods` set.
    pub fn new_with(mut config: DavConfig) -> DavHandler {
        config.reqhooks = None;
        DavHandler{ config: Arc::new(config) }
    }

    /// No matter how many `DavHandler`s are created, they all run on a single
    /// shared threadpool. The default size is `8` threads. With this function, you
    /// can change the number of threads, but only if it is called BEFORE any
    /// requests are served.
    pub fn num_threads(num: usize) {
        sync_adapter::num_threads(num)
    }

    /// Handle a webdav request.
    ///
    /// Only one error kind is ever returned: `ErrorKind::BrokenPipe`. In that case we
    /// were not able to generate a response at all, and the server should just
    /// close the connection.
    pub fn handle<ReqBody, ReqError>(&self, req: http::Request<ReqBody>)
      -> impl Future<Item = http::Response<BoxedByteStream>, Error = std::io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: ToString,
    {
        if self.config.fs.is_none() {
            return future::Either::A(notfound());
        }
        let mut inner = DavInner::from(&*self.config);
        let fut = sync_adapter::handler(req, move |req, resp| {
            inner.handle(req, resp)
        });
        future::Either::B(fut)
    }

    /// Handle a webdav request, overriding parts of the config.
    ///
    /// For example, the `principal` can be set for this request.
    ///
    /// Or, the default config has no locksystem, and you pass in
    /// a fake locksystem (`FakeLs`) because this is a request from a
    /// windows or osx client that needs to see locking support.
    pub fn handle_with<ReqBody, ReqError>(&self, config: DavConfig, req: http::Request<ReqBody>)
      -> impl Future<Item = http::Response<BoxedByteStream>, Error = std::io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: ToString,
    {
        let orig = &*self.config;
        let newconf = DavConfig {
            prefix: config.prefix.or(orig.prefix.clone()),
            fs: config.fs.or(orig.fs.clone()),
            ls: config.ls.or(orig.ls.clone()),
            allow: config.allow.or(orig.allow.clone()),
            principal: config.principal.or(orig.principal.clone()),
            reqhooks: config.reqhooks,
        };
        if newconf.fs.is_none() {
            return future::Either::A(notfound());
        }
        let mut inner = DavInner::from(newconf);
        let fut = sync_adapter::handler(req, move |req, resp| {
            inner.handle(req, resp)
        });
        future::Either::B(fut)
    }
}

impl DavInner {

    // helper.
    pub(crate) fn has_parent(&self, path: &WebPath) -> bool {
        let p = path.parent();
        self.fs.metadata(&p).map(|m| m.is_dir()).unwrap_or(false)
    }

    // helper.
    pub(crate) fn path(&self, req: &Request) -> WebPath {
        // This never fails (has been checked before)
        WebPath::from_uri(&req.uri, &self.prefix).unwrap()
    }

    // See if this is a directory and if so, if we have
    // to fixup the path by adding a slash at the end.
    pub(crate) fn fixpath(&self, req: &Request, res: &mut Response) -> FsResult<(WebPath, Box<DavMetaData>)> {
        let mut path = self.path(&req);
        let meta = self.fs.metadata(&path)?;
        if meta.is_dir() && !path.is_collection() {
            path.add_slash();
            let newloc = path.as_url_string_with_prefix();
            res.headers_mut().typed_insert(headers::ContentLocation(newloc));
        }
        Ok((path, meta))
    }

    // drain request body and return length.
    pub(crate) fn drain_request(&self, req: &mut Request) -> usize {
        let mut buffer = [0; 8192];
        let mut done = 0;
        loop {
            match req.read(&mut buffer[..]) {
                Ok(n) if n > 0 => done += n,
                _ => break,
            }
        }
        done
    }

    // internal dispatcher.
    fn handle(&mut self, mut req: Request, mut res: Response) {

        // run hooks at start and end of request.
        let mut dropper = Dropper(None);
        let mut x = None;
        std::mem::swap(&mut self.reqhooks, &mut x);
        if let Some((starthook, stophook)) = x {
            dropper.0.get_or_insert(stophook);
            starthook();
        }

        // debug when running the webdav litmus tests.
        if log_enabled!(log::Level::Debug) {
            if let Some(t) = req.headers.typed_get::<headers::XLitmus>() {
                debug!("X-Litmus: {}", t);
            }
        }

        // translate HTTP method to Webdav method.
        let method = match dav_method(&req.method) {
            Ok(m) => m,
            Err(e) => {
                debug!("refusing method {} request {}", &req.method, &req.uri);
                res.headers_mut().typed_insert(typed_headers::Connection::close());
                *res.status_mut() = e.statuscode();
                return;
            },
        };

        // see if method is allowed.
        if let Some(ref a) = self.allow {
            if !a.allowed(method) {
                debug!("method {} not allowed on request {}", req.method, req.uri);
                res.headers_mut().typed_insert(typed_headers::Connection::close());
                *res.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
                return;
            }
        }

        // make sure the request path is valid.
        let path = match WebPath::from_uri(&req.uri, &self.prefix) {
            Ok(p) => p,
            Err(e) => { 
                res.headers_mut().typed_insert(typed_headers::Connection::close());
                daverror(&mut res, e);
                return;
            },
        };

        // some handlers expect a body, but most do not, so just drain
        // the body here first. If there was a body, reject request
        // with Unsupported Media Type.
        match method {
            Method::Patch |
            Method::Put |
            Method::PropFind |
            Method::PropPatch |
            Method::Lock => {},
            _ => {
                if self.drain_request(&mut req) > 0 {
                    *res.status_mut() = StatusCode::UNSUPPORTED_MEDIA_TYPE;
                    return;
                }
            }
        }

        debug!("== START REQUEST {:?} {}", method, path);
        if let Err(e) = match method {
            Method::Head | Method::Get => self.handle_get(req, res),
            Method::Put | Method::Patch => self.handle_put(req, res),
            Method::Options => self.handle_options(req, res),
            Method::PropFind => self.handle_propfind(req, res),
            Method::PropPatch => self.handle_proppatch(req, res),
            Method::MkCol => self.handle_mkcol(req, res),
            Method::Copy => self.handle_copymove(method, req, res),
            Method::Move => self.handle_copymove(method, req, res),
            Method::Delete => self.handle_delete(req, res),
            Method::Lock => self.handle_lock(req, res),
            Method::Unlock => self.handle_unlock(req, res),
        } {
            debug!("== END REQUEST result {:?}", e);
        } else {
            debug!("== END REQUEST result OK");
        }
    }
}
