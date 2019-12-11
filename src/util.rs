use std::time::{SystemTime, UNIX_EPOCH};

use headers::Header;
use http::method::InvalidMethod;

use crate::body::Body;
use crate::errors::DavError;
use crate::DavResult;

/// HTTP Methods supported by DavHandler.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
#[repr(u32)]
pub enum DavMethod {
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
pub(crate) fn dav_method(m: &http::Method) -> DavResult<DavMethod> {
    let m = match m {
        &http::Method::HEAD => DavMethod::Head,
        &http::Method::GET => DavMethod::Get,
        &http::Method::PUT => DavMethod::Put,
        &http::Method::PATCH => DavMethod::Patch,
        &http::Method::DELETE => DavMethod::Delete,
        &http::Method::OPTIONS => DavMethod::Options,
        _ => {
            match m.as_str() {
                "PROPFIND" => DavMethod::PropFind,
                "PROPPATCH" => DavMethod::PropPatch,
                "MKCOL" => DavMethod::MkCol,
                "COPY" => DavMethod::Copy,
                "MOVE" => DavMethod::Move,
                "LOCK" => DavMethod::Lock,
                "UNLOCK" => DavMethod::Unlock,
                _ => {
                    return Err(DavError::UnknownDavMethod);
                },
            }
        },
    };
    Ok(m)
}

// for external use.
impl std::convert::TryFrom<&http::Method> for DavMethod {
    type Error = InvalidMethod;

    fn try_from(value: &http::Method) -> Result<Self, Self::Error> {
        dav_method(value).map_err(|_| {
            // A trick to get at the value of http::method::InvalidMethod.
            http::method::Method::from_bytes(b"").unwrap_err()
        })
    }
}

/// A set of allowed [`DavMethod`]s.
///
/// [`DavMethod`]: enum.DavMethod.html
#[derive(Clone, Copy, Debug)]
pub struct DavMethodSet(u32);

impl DavMethodSet {
    /// New set, all methods allowed.
    pub fn all() -> DavMethodSet {
        DavMethodSet(0xffffffff)
    }

    /// New empty set.
    pub fn none() -> DavMethodSet {
        DavMethodSet(0)
    }

    /// Add a method.
    pub fn add(&mut self, m: DavMethod) -> &Self {
        self.0 |= m as u32;
        self
    }

    /// Remove a method.
    pub fn remove(&mut self, m: DavMethod) -> &Self {
        self.0 &= !(m as u32);
        self
    }

    /// Check if a method is in the set.
    pub fn contains(&self, m: DavMethod) -> bool {
        self.0 & (m as u32) > 0
    }

    /// Generate an DavMethodSet from a list of words.
    pub fn from_vec(v: Vec<impl AsRef<str>>) -> Result<DavMethodSet, InvalidMethod> {
        const HTTP_RO: u32 = DavMethod::Get as u32 | DavMethod::Head as u32 | DavMethod::Options as u32;
        const HTTP_RW: u32 = HTTP_RO | DavMethod::Put as u32;
        const WEBDAV_RO: u32 = HTTP_RO | DavMethod::PropFind as u32;
        const WEBDAV_RW: u32 = 0xffffffff;

        let mut m: u32 = 0;
        for w in &v {
            m |= match w.as_ref().to_lowercase().as_str() {
                "head" => DavMethod::Head as u32,
                "get" => DavMethod::Get as u32,
                "put" => DavMethod::Put as u32,
                "patch" => DavMethod::Patch as u32,
                "delete" => DavMethod::Delete as u32,
                "options" => DavMethod::Options as u32,
                "propfind" => DavMethod::PropFind as u32,
                "proppatch" => DavMethod::PropPatch as u32,
                "mkcol" => DavMethod::MkCol as u32,
                "copy" => DavMethod::Copy as u32,
                "move" => DavMethod::Move as u32,
                "lock" => DavMethod::Lock as u32,
                "unlock" => DavMethod::Unlock as u32,
                "http-ro" => HTTP_RO,
                "http-rw" => HTTP_RW,
                "webdav-ro" => WEBDAV_RO,
                "webdav-rw" => WEBDAV_RW,
                _ => {
                    // A trick to get at the value of http::method::InvalidMethod.
                    let invalid_method = http::method::Method::from_bytes(b"").unwrap_err();
                    return Err(invalid_method);
                },
            };
        }
        Ok(DavMethodSet(m))
    }
}

pub(crate) fn dav_xml_error(body: &str) -> Body {
    let xml = format!(
        "{}\n{}\n{}\n{}\n",
        r#"<?xml version="1.0" encoding="utf-8" ?>"#, r#"<D:error xmlns:D="DAV:">"#, body, r#"</D:error>"#
    );
    Body::from(xml)
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
