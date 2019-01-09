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
//! Example:
//!
//! ```
//! use hyper;
//! use bytes::Bytes;
//! use futures::{future::Future, stream::Stream};
//! use webdav_handler::{DavHandler, localfs::LocalFs, memls::MemLs};
//!
//! fn main() {
//!     let dir = "/tmp";
//!     let addr = ([127, 0, 0, 1], 4918).into();
//!
//!     let dav_server = DavHandler::new("", None, LocalFs::new(dir, false), Some(MemLs::new()));
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
use std::collections::HashSet;
use std::sync::Arc;

use bytes;
use futures::{Future, Stream};

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
pub enum Method {
    Head,
    Get,
    Put,
    Patch,
    Options,
    PropFind,
    PropPatch,
    MkCol,
    Copy,
    Move,
    Delete,
    Lock,
    Unlock,
}

/// The webdav handler struct.
#[derive(Clone)]
pub struct DavHandler(Arc<DavInner>);

// The actual struct.
pub(crate) struct DavInner {
    pub prefix:     String,
    pub fs:         Box<DavFileSystem>,
    pub ls:         Option<Box<DavLockSystem>>,
    pub allow:      Option<HashSet<Method>>,
    pub principal:  Option<String>,
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

impl DavHandler {
    /// Handle a webdav request.
    ///
    /// Only one error kind is ever returned: ErrorKind::BrokenPipe. In that case we
    /// were not able to generate a response at all, and the server should just
    /// close the connection.
    pub fn handle<ReqBody, ReqError>(&self, req: http::Request<ReqBody>)
      -> impl Future<Item = http::Response<BoxedByteStream>, Error = std::io::Error>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: ToString,
    {
        let inner = self.0.clone();
        sync_adapter::handler(req, move |req, resp| {
            inner.handle(req, resp)
        })
    }

    /// Create a new DavHandler.
    ///
    /// prefix | The prefix to be stripped from the request URL
    /// user   | Optional username (or principal) of the requesting entity. Used with locking.
    /// fs     | The filesystem backend.
    /// ls     | Optional locksystem backend
    ///
    pub fn new(prefix: impl Into<String>, user: Option<&str>, fs: Box<DavFileSystem>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        let inner = DavInner{
            prefix: prefix.into(),
            fs: fs,
            ls: ls,
            allow: None,
            principal: user.map(|s| s.to_string()),
        };
        DavHandler(Arc::new(inner))
    }

    /// Clone an existing handler, and possibly override any of its properties.
    /// Note that the allowed method set is not copied (it is set to "all" again).
    pub fn clone_with(&self, prefix: Option<&str>, user: Option<&str>, fs: Option<Box<DavFileSystem>>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        let inner = DavInner{
            prefix: prefix.map(|s| s.into()).unwrap_or(self.0.prefix.clone()),
            fs: fs.unwrap_or(self.0.fs.clone()),
            ls: ls.or(self.0.ls.clone()),
            allow: None,
            principal: user.map(|s| s.to_string()).or(self.0.principal.clone()),
        };
        DavHandler(Arc::new(inner))
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
    fn handle(&self, mut req: Request, mut res: Response) {

        // debug when running the webdav litmus tests.
        //if log_enabled!(log::Level::Debug) {
            if let Some(t) = req.headers.typed_get::<headers::XLitmus>() {
                debug!("X-Litmus: {}", t);
                debug!("headers: {:?}", req.headers);
            }
            if let Some(t) = req.headers.typed_get::<typed_headers::Authorization<typed_headers::Basic>>() {
                debug!("Authorization (typed): {:?}", t);
            }
            if let Some(t) = req.headers.get("Authorization") {
                debug!("Authorization (normal): {:?}", t);
            }
        //}

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
            if !a.contains(&method) {
                debug!("method {} not allowed on request {}", &req.method, &req.uri);
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

    /// Only allow certain methods. By default, all methods are allowed, and
    /// advertised in the Allow: DAV header. You need to call this function
    /// multiple times, for every method you want to allow, but the calls
    /// can be chained in a builder-like pattern.
    ///
    /// ```
    /// let dav = DavHandler::new(....)
    ///     .allow(dav::Method::Get)
    ///     .allow(dav::Method::PropFind)
    ///     .allow(dav::Method::Options);
    /// ```
    ///
    /// This needs to be replaced by something like a `MethodSet`.
    pub fn allow(mut self, m: Method) -> DavInner {
        match self.allow {
            Some(ref mut a) => { a.insert(m); },
            None => {
                let mut h = HashSet::new();
                h.insert(m);
                self.allow = Some(h);
            },
        }
        self
    }
}
