use std::io::{Cursor, Write};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use headers::Header;
use http::method::InvalidMethod;
use time::macros::offset;
use time::format_description::well_known::Rfc3339;

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
    pub const HTTP_RO: DavMethodSet =
        DavMethodSet(DavMethod::Get as u32 | DavMethod::Head as u32 | DavMethod::Options as u32);
    pub const HTTP_RW: DavMethodSet = DavMethodSet(Self::HTTP_RO.0 | DavMethod::Put as u32);
    pub const WEBDAV_RO: DavMethodSet = DavMethodSet(Self::HTTP_RO.0 | DavMethod::PropFind as u32);
    pub const WEBDAV_RW: DavMethodSet = DavMethodSet(0xffffffff);

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
                "http-ro" => Self::HTTP_RO.0,
                "http-rw" => Self::HTTP_RW.0,
                "webdav-ro" => Self::WEBDAV_RO.0,
                "webdav-rw" => Self::WEBDAV_RW.0,
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

pub(crate) fn systemtime_to_offsetdatetime(t: SystemTime) -> time::OffsetDateTime {
    match t.duration_since(UNIX_EPOCH) {
        Ok(t) => {
            let tm = time::OffsetDateTime::from_unix_timestamp(t.as_secs() as i64).unwrap();
            tm.to_offset(offset!(UTC))
        },
        Err(_) => time::OffsetDateTime::UNIX_EPOCH.to_offset(offset!(UTC)),
    }
}

pub(crate) fn systemtime_to_httpdate(t: SystemTime) -> String {
    let d = headers::Date::from(t);
    let mut v = Vec::new();
    d.encode(&mut v);
    v[0].to_str().unwrap().to_owned()
}

pub(crate) fn systemtime_to_rfc3339(t: SystemTime) -> String {
    // 1996-12-19T16:39:57Z
    systemtime_to_offsetdatetime(t).format(&Rfc3339).unwrap()
}

// A buffer that implements "Write".
#[derive(Clone)]
pub(crate) struct MemBuffer(Cursor<Vec<u8>>);

impl MemBuffer {
    pub fn new() -> MemBuffer {
        MemBuffer(Cursor::new(Vec::new()))
    }

    pub fn take(&mut self) -> Bytes {
        let buf = std::mem::replace(self.0.get_mut(), Vec::new());
        self.0.set_position(0);
        Bytes::from(buf)
    }
}

impl Write for MemBuffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

    #[test]
    fn test_rfc3339() {
        assert!(systemtime_to_rfc3339(UNIX_EPOCH) == "1970-01-01T00:00:00Z");
    }
}
