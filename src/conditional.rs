use std::time::{Duration, SystemTime, UNIX_EPOCH};

use headers::HeaderMapExt;
use http::{Method, StatusCode};

use crate::davheaders::{self, ETag};
use crate::davpath::DavPath;
use crate::fs::{DavMetaData, GuardedFileSystem};
use crate::ls::DavLockSystem;

type Request = http::Request<()>;

// SystemTime has nanosecond precision. Round it down to the
// nearest second, because an HttpDate has second precision.
fn round_time(tm: impl Into<SystemTime>) -> SystemTime {
    let tm = tm.into();
    match tm.duration_since(UNIX_EPOCH) {
        Ok(d) => UNIX_EPOCH + Duration::from_secs(d.as_secs()),
        Err(_) => tm,
    }
}

pub(crate) fn ifrange_match(
    hdr: &davheaders::IfRange,
    tag: Option<&davheaders::ETag>,
    date: Option<SystemTime>,
) -> bool {
    match *hdr {
        davheaders::IfRange::Date(ref d) => match date {
            Some(date) => round_time(date) == round_time(*d),
            None => false,
        },
        davheaders::IfRange::ETag(ref t) => match tag {
            Some(tag) => t == tag,
            None => false,
        },
    }
}

pub(crate) fn etaglist_match(
    tags: &davheaders::ETagList,
    exists: bool,
    tag: Option<&davheaders::ETag>,
) -> bool {
    match *tags {
        davheaders::ETagList::Star => exists,
        davheaders::ETagList::Tags(ref t) => match tag {
            Some(tag) => t.iter().any(|x| x == tag),
            None => false,
        },
    }
}

// Handle the if-headers: RFC 7232, HTTP/1.1 Conditional Requests.
pub(crate) fn http_if_match(
    req: &Request,
    meta: Option<&Box<dyn DavMetaData>>,
) -> Option<StatusCode> {
    let file_modified = meta.and_then(|m| m.modified().ok());

    if let Some(r) = req.headers().typed_get::<davheaders::IfMatch>() {
        let etag = meta.and_then(ETag::from_meta);
        if !etaglist_match(&r.0, meta.is_some(), etag.as_ref()) {
            trace!("precondition fail: If-Match {:?}", r);
            return Some(StatusCode::PRECONDITION_FAILED);
        }
    } else if let Some(r) = req.headers().typed_get::<headers::IfUnmodifiedSince>() {
        match file_modified {
            None => return Some(StatusCode::PRECONDITION_FAILED),
            Some(file_modified) => {
                if round_time(file_modified) > round_time(r) {
                    trace!("precondition fail: If-Unmodified-Since {:?}", r);
                    return Some(StatusCode::PRECONDITION_FAILED);
                }
            }
        }
    }

    if let Some(r) = req.headers().typed_get::<davheaders::IfNoneMatch>() {
        let etag = meta.and_then(ETag::from_meta);
        if etaglist_match(&r.0, meta.is_some(), etag.as_ref()) {
            trace!("precondition fail: If-None-Match {:?}", r);
            if req.method() == Method::GET || req.method() == Method::HEAD {
                return Some(StatusCode::NOT_MODIFIED);
            } else {
                return Some(StatusCode::PRECONDITION_FAILED);
            }
        }
    } else if let Some(r) = req.headers().typed_get::<headers::IfModifiedSince>() {
        if req.method() == Method::GET || req.method() == Method::HEAD {
            if let Some(file_modified) = file_modified {
                if round_time(file_modified) <= round_time(r) {
                    trace!("not-modified If-Modified-Since {:?}", r);
                    return Some(StatusCode::NOT_MODIFIED);
                }
            }
        }
    }
    None
}

// handle the If header: RFC4918, 10.4.  If Header
//
// returns true if the header was not present, or if any of the iflists
// evaluated to true. Also returns a Vec of StateTokens that we encountered.
//
// caller should set the http status to 412 PreconditionFailed if
// the return value from this function is false.
//
pub(crate) async fn dav_if_match<'a, C>(
    req: &'a Request,
    fs: &'a (dyn GuardedFileSystem<C> + 'static),
    ls: &'a Option<Box<dyn DavLockSystem + 'static>>,
    path: &'a DavPath,
    credentials: &C,
) -> (bool, Vec<String>)
where
    C: Clone + Send + Sync + 'static,
{
    let mut tokens: Vec<String> = Vec::new();
    let mut any_list_ok = false;

    let r = match req.headers().typed_get::<davheaders::If>() {
        Some(r) => r,
        None => return (true, tokens),
    };

    for iflist in r.0.iter() {
        // save and return all statetokens that we encountered.
        let toks = iflist.conditions.iter().filter_map(|c| match c.item {
            davheaders::IfItem::StateToken(ref t) => Some(t.to_owned()),
            _ => None,
        });
        tokens.extend(toks);

        // skip over if a previous list already evaluated to true.
        if any_list_ok {
            continue;
        }

        // find the resource that this list is about.
        let mut pa: Option<DavPath> = None;
        let (p, valid) = match iflist.resource_tag {
            Some(ref url) => {
                match DavPath::from_str_and_prefix(url.path(), path.prefix()) {
                    Ok(p) => {
                        // anchor davpath in pa.
                        let p: &DavPath = pa.get_or_insert(p);
                        (p, true)
                    }
                    Err(_) => (path, false),
                }
            }
            None => (path, true),
        };

        // now process the conditions. they must all be true.
        let mut list_ok = false;
        for cond in iflist.conditions.iter() {
            let cond_ok = match cond.item {
                davheaders::IfItem::StateToken(ref s) => {
                    // tokens in DAV: namespace always evaluate to false (10.4.8)
                    if !valid || s.starts_with("DAV:") {
                        false
                    } else {
                        match *ls {
                            Some(ref ls) => ls.check(p, None, true, false, vec![s]).is_ok(),
                            None => false,
                        }
                    }
                }
                davheaders::IfItem::ETag(ref tag) => {
                    if !valid {
                        // invalid location, so always false.
                        false
                    } else {
                        match fs.metadata(p, credentials).await {
                            Ok(meta) => {
                                // exists and may have metadata ..
                                if let Some(mtag) = ETag::from_meta(meta) {
                                    tag == &mtag
                                } else {
                                    false
                                }
                            }
                            Err(_) => {
                                // metadata error, fail.
                                false
                            }
                        }
                    }
                }
            };
            if cond_ok == cond.not {
                list_ok = false;
                break;
            }
            list_ok = true;
        }
        if list_ok {
            any_list_ok = true;
        }
    }
    if !any_list_ok {
        trace!("precondition fail: If {:?}", r.0);
    }
    (any_list_ok, tokens)
}

// Handle both the HTTP conditional If: headers, and the webdav If: header.
pub(crate) async fn if_match<'a, C>(
    req: &'a Request,
    meta: Option<&'a Box<dyn DavMetaData + 'static>>,
    fs: &'a (dyn GuardedFileSystem<C> + 'static),
    ls: &'a Option<Box<dyn DavLockSystem + 'static>>,
    path: &'a DavPath,
    credentials: &C,
) -> Option<StatusCode>
where
    C: Clone + Send + Sync + 'static,
{
    match dav_if_match(req, fs, ls, path, credentials).await {
        (true, _) => {}
        (false, _) => return Some(StatusCode::PRECONDITION_FAILED),
    }
    http_if_match(req, meta)
}

// Like if_match, but also returns all "associated state-tokens"
pub(crate) async fn if_match_get_tokens<'a, C>(
    req: &'a Request,
    meta: Option<&'a Box<dyn DavMetaData + 'static>>,
    fs: &'a (dyn GuardedFileSystem<C> + 'static),
    ls: &'a Option<Box<dyn DavLockSystem + 'static>>,
    path: &'a DavPath,
    credentials: &C,
) -> Result<Vec<String>, StatusCode>
where
    C: Clone + Send + Sync + 'static,
{
    if let Some(code) = http_if_match(req, meta) {
        return Err(code);
    }
    match dav_if_match(req, fs, ls, path, credentials).await {
        (true, v) => Ok(v),
        (false, _) => Err(StatusCode::PRECONDITION_FAILED),
    }
}
