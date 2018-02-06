
use hyper;
use hyper::server::Request;
use hyper::status::StatusCode;
use hyper::method::Method;
use hyper::header::EntityTag;

use super::headers;
use super::systemtime_to_timespec;
use super::fs::DavMetaData;

// handle the if-headers: RFC 7232, HTTP/1.1 Conditional Requests
pub(crate) fn ifmatch(req: &Request, meta: Option<&Box<DavMetaData>>) -> Option<StatusCode> {

    let modified = meta.and_then(|m| m.modified().ok());

    if let Some(r) = req.headers.get::<headers::IfMatch>() {
        let etag = meta.map(|m| EntityTag::new(false, m.etag()));
        if etag.map_or(true, |m| !r.matches(&m)) {
            return Some(StatusCode::PreconditionFailed);
        }
    } else if let Some(r) = req.headers.get::<hyper::header::IfUnmodifiedSince>() {
        match modified {
            None => return Some(StatusCode::PreconditionFailed),
            Some(m) => {
                let ts = systemtime_to_timespec(m);
                if ts > (r.0).0.to_timespec() {
                    return Some(StatusCode::PreconditionFailed);
                }
            }
        }
    }

    if let Some(r) = req.headers.get::<headers::IfNoneMatch>() {
        let etag = meta.map(|m| EntityTag::new(false, m.etag()));
        if etag.map_or(false, |m| r.matches(&m)) {
            return Some(StatusCode::PreconditionFailed);
        }
    } else if let Some(r) = req.headers.get::<hyper::header::IfModifiedSince>() {
        if req.method == Method::Get || req.method == Method::Head {
            if let Some(m) = modified {
                let ts = systemtime_to_timespec(m);
                if ts > (r.0).0.to_timespec() {
                    return Some(StatusCode::NotModified);
                }
            }
        }
    }
    None
}
