use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::{self, Bytes};

use futures::stream::TryStreamExt;
use futures01;

use headers::Header;
use http::Method as httpMethod;

use crate::errors::DavError;
use crate::{BoxedByteStream, DavResult};

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

/// A set of allowed [`Method`]s.
///
/// [`Method`]: enum.Method.html
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
pub(crate) fn notfound() -> impl futures01::Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
{
    let body = futures01::stream::once(Ok(bytes::Bytes::from("Not Found")));
    let body: BoxedByteStream = Box::new(body);
    let response = http::Response::builder()
        .status(404)
        .header("connection", "close")
        .body(body)
        .unwrap();
    return Box::new(futures01::future::ok(response));
}

// helper.
pub(crate) fn empty_body() -> BoxedByteStream {
    Box::new(futures::stream::empty::<io::Result<Bytes>>().compat())
}

pub(crate) fn single_body(body: impl Into<Bytes>) -> BoxedByteStream {
    let body = vec![Ok::<Bytes, io::Error>(body.into())].into_iter();
    Box::new(futures::stream::iter(body).compat())
}

pub(crate) fn dav_xml_error(body: &str) -> BoxedByteStream {
    let xml = format!(
        "{}\n{}\n{}\n{}\n",
        r#"<?xml version="1.0" encoding="utf-8" ?>"#, r#"<D:error xmlns:D="DAV:">"#, body, r#"</D:error>"#
    );
    single_body(xml)
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

pub(crate) fn systemtime_to_httpdate(t: SystemTime) -> String {
    let d = headers::Date::from(t);
    let mut v = Vec::new();
    d.encode(&mut v);
    v[0].to_str().unwrap().to_owned()
}

pub(crate) fn systemtime_to_rfc3339(t: SystemTime) -> String {
    let ts = systemtime_to_timespec(t);
    format!("{}", time::at_utc(ts).rfc3339())
}
