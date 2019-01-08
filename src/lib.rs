//!
//! A webdav handler for the Rust "hyper" HTTP server library. Uses an
//! interface similar to the Go x/net/webdav package:
//!
//! - the library contains an HTTP handler (for Hyper 0.10.x at the moment)
//! - you supply a "filesystem" for backend storage, which can optionally
//!   implement reading/writing "DAV properties"
//! - you can supply a "locksystem" that handles the webdav locks
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
//! extern crate hyper;
//! extern crate webdav_handler as dav;
//!
//! struct SampleServer {
//!     fs:     Box<dav::DavFileSystem>,
//!     ls:     Box<dav::DavLockSystem>,
//!     prefix: String,
//! }
//!
//! impl Handler for SampleServer {
//!     fn handle(&self, req: hyper::server::Request, mut res: hyper::server::Response) {
//!         let davhandler = dav::DavHandler::new(&self.prefix, self.fs.clone(), self.ls.clone());
//!         davhandler.handle(req, res);
//!     }
//! }
//!
//! fn main() {
//!     let sample_srv = SampleServer{
//!         fs:     dav::memfs::MemFs::new(),
//!         ls:     dav::memls::MemLs::new(),
//!         prefix: "".to_string(),
//!     };
//!     let hyper_srv = hyper::server::Server::http("0.0.0.0:4918").unwrap();
//!     hyper_srv.handle_threads(sample_srv, 8).unwrap();
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

mod typed_headers;
mod sync_adapter;

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

use crate::typed_headers::{Date, HeaderMapExt};
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
    pub(crate) prefix:     String,
    pub(crate) fs:         Box<DavFileSystem>,
    pub(crate) ls:         Option<Box<DavLockSystem>>,
    pub(crate) allow:      Option<HashSet<Method>>,
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

    // constructor.
    pub fn new<S: Into<String>>(prefix: S, fs: Box<DavFileSystem>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        let inner = DavInner{
            prefix: prefix.into(),
            fs: fs,
            ls: ls,
            allow: None,
        };
        DavHandler(Arc::new(inner))
    }
}

impl DavInner {

    pub(crate) fn has_parent(&self, path: &WebPath) -> bool {
        let p = path.parent();
        self.fs.metadata(&p).map(|m| m.is_dir()).unwrap_or(false)
    }

    pub(crate) fn path(&self, req: &Request) -> WebPath {
        // XXX FIXME need to make sure this never fails
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

    pub(crate) fn drain_request(&self, req: &mut Request) -> usize {
        let (_, done) = self.do_read_request_max(req, 0);
        done
    }

    pub(crate) fn read_request_max(&self, req: &mut Request, max: usize) -> Vec<u8> {
        let (v, _) = self.do_read_request_max(req, max);
        v
    }

    pub(crate) fn do_read_request_max(&self, req: &mut Request, max: usize) -> (Vec<u8>, usize) {
        let mut v = Vec::new();
        let mut buffer = [0; 8192];
        let mut done = 0;
        loop {
            match req.read(&mut buffer[..]) {
                Ok(n) if n > 0 => {
                    if v.len() < max {
                        v.extend_from_slice(&buffer[..n]);
                    }
                    done += n;
                }
                _ => break,
            }
        }
        (v, done)
    }

    // dispatcher.
    fn handle(&self, mut req: Request, mut res: Response) {

        // XXX FIXME what does this do? Is it for webdav litmus?
        if let None = req.headers.typed_get::<Date>() {
            let now = SystemTime::now();
            res.headers_mut().typed_insert(Date(typed_headers::HttpDate::from(now)));
        }

        if let Some(t) = req.headers.typed_get::<headers::XLitmus>() {
            debug!("X-Litmus: {}", t);
        }

        let method = match dav_method(&req.method) {
            Ok(m) => m,
            Err(e) => {
                debug!("refusing method {} request {}", &req.method, &req.uri);
                res.headers_mut().typed_insert(typed_headers::Connection::close());
                *res.status_mut() = e.statuscode();
                return;
            },
        };

        if let Some(ref a) = self.allow {
            if !a.contains(&method) {
                debug!("method {} not allowed on request {}", &req.method, &req.uri);
                res.headers_mut().typed_insert(typed_headers::Connection::close());
                *res.status_mut() = StatusCode::METHOD_NOT_ALLOWED;
                return;
            }
        }

        // make sure the request path is valid.
        // XXX why do this twice ... oh well.
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
