//!
//! A webdav handler for the Rust "hyper" HTTP server library. Uses an
//! interface similar to the Go x/net/webdav package:
//!
//! - the library contains an HTTP handler (for Hyper 0.10.x at the moment)
//! - you supply a "filesystem" for backend storage, which can optionally
//!   implement reading/writing "DAV properties"
//! - you can supply a "locksystem" that handles the webdav locks
//!
//! Currently passes the "basic", "copymove", "props" and "http"
//! checks of the Webdav Litmus Test testsuite (so basically all of
//! RFC4918 except locking).
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

#[macro_use] extern crate hyper;
#[macro_use] extern crate log;
#[macro_use] extern crate lazy_static;

extern crate serde;
extern crate env_logger;
extern crate regex;
extern crate xml;
extern crate libc;
extern crate time;
extern crate sha2;
extern crate url;
extern crate xmltree;
extern crate uuid;
extern crate mime_guess;
extern crate filetime;
extern crate htmlescape;

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

pub mod fs;
pub mod ls;
pub mod localfs;
pub mod memfs;
pub mod memls;
pub mod fakels;
pub mod webpath;

use hyper::header::Date;
use hyper::server::{Request, Response};
use hyper::method::Method as httpMethod;
use hyper::status::StatusCode;

use std::io::Read;
use std::time::{UNIX_EPOCH,SystemTime};
use std::collections::HashSet;

use self::webpath::WebPath;

pub(crate) use self::errors::DavError;
pub(crate) use self::fs::*;
pub(crate) use self::ls::*;

type DavResult<T> = Result<T, DavError>;

/// Methods supported by DavHandler.
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
//#[derive(Debug)]
pub struct DavHandler {
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

pub(crate) fn systemtime_to_httpdate(t: SystemTime) -> hyper::header::HttpDate {
    let ts = systemtime_to_timespec(t);
    hyper::header::HttpDate(time::at_utc(ts))
}

pub(crate) fn systemtime_to_rfc3339(t: SystemTime) -> String {
    let ts = systemtime_to_timespec(t);
    format!("{}", time::at_utc(ts).rfc3339())
}

// translate method into our own enum that has webdav methods as well.
pub(crate) fn dav_method(m: &hyper::method::Method) -> DavResult<Method> {
    let m = match m {
        &httpMethod::Head => Method::Head,
        &httpMethod::Get => Method::Get,
        &httpMethod::Put => Method::Put,
        &httpMethod::Patch => Method::Patch,
        &httpMethod::Delete => Method::Delete,
        &httpMethod::Options => Method::Options,
        &httpMethod::Extension(ref s) if s == "PROPFIND" => Method::PropFind,
        &httpMethod::Extension(ref s) if s == "PROPPATCH" => Method::PropPatch,
        &httpMethod::Extension(ref s) if s == "MKCOL" => Method::MkCol,
        &httpMethod::Extension(ref s) if s == "COPY" => Method::Copy,
        &httpMethod::Extension(ref s) if s == "MOVE" => Method::Move,
        &httpMethod::Extension(ref s) if s == "LOCK" => Method::Lock,
        &httpMethod::Extension(ref s) if s == "UNLOCK" => Method::Unlock,
        _ => {
            return Err(DavError::UnknownMethod);
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
        FsError::NotImplemented => StatusCode::NotImplemented,
        FsError::GeneralFailure => StatusCode::InternalServerError,
        FsError::Exists => StatusCode::MethodNotAllowed,
        FsError::NotFound => StatusCode::NotFound,
        FsError::Forbidden => StatusCode::Forbidden,
        FsError::InsufficientStorage => StatusCode::InsufficientStorage,
        FsError::LoopDetected => StatusCode::LoopDetected,
        FsError::PathTooLong => StatusCode::UriTooLong,
        FsError::TooLarge => StatusCode::PayloadTooLarge,
        FsError::IsRemote => StatusCode::BadGateway,
    }
}

impl DavHandler {

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
            res.headers_mut().set(headers::ContentLocation(newloc));
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
    pub fn handle(&self, mut req: Request, mut res: Response) {

        // enable TCP_NODELAY
        if let Some(httpstream) = req.downcast_ref::<hyper::net::HttpStream>() {
            httpstream.0.set_nodelay(true).ok();
        }

        if let None = req.headers.get::<Date>() {
            let now = time::now();
            res.headers_mut().set(Date(hyper::header::HttpDate(now)));
        }
        if let Some(t) = req.headers.get::<headers::XLitmus>() {
            debug!("X-Litmus: {}", t);
        }
        let method = match dav_method(&req.method) {
            Ok(m) => m,
            Err(e) => {
                debug!("refusing method {} request {}", &req.method, &req.uri);
                *res.status_mut() = e.statuscode();
                return;
            },
        };

        if let Some(ref a) = self.allow {
            if !a.contains(&method) {
                debug!("method {} not allowed on request {}", &req.method, &req.uri);
                *res.status_mut() = StatusCode::MethodNotAllowed;
                return;
            }
        }

        // make sure the request path is valid.
        // XXX why do this twice ... oh well.
        let path = match WebPath::from_uri(&req.uri, &self.prefix) {
            Ok(p) => p,
            Err(e) => { daverror(&mut res, e); return; },
        };

        // some handlers expect a body, but most do not, so just drain
        // the body here first. If there was a body, reject request
        // with Unspoorted Media Type.
        match method {
            Method::Put |
            Method::PropFind |
            Method::PropPatch |
            Method::Lock => {},
            _ => {
                if self.drain_request(&mut req) > 0 {
                    *res.status_mut() = StatusCode::UnsupportedMediaType;
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

    pub fn allow(mut self, m: Method) -> DavHandler {
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

    // constructor.
    pub fn new<S: Into<String>>(prefix: S, fs: Box<DavFileSystem>, ls: Option<Box<DavLockSystem>>) -> DavHandler {
        DavHandler{
            prefix: prefix.into(),
            fs: fs,
            ls: ls,
            allow: None,
        }
    }
}
