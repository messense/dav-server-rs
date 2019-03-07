use std::time::SystemTime;

use http::StatusCode;
use http::{self, Method};

use crate::davheaders;
use crate::fs::{DavFileSystem, DavMetaData};
use crate::ls::DavLockSystem;
use crate::typed_headers::{self, EntityTag, HeaderMapExt};
use crate::webpath::WebPath;

type Request = http::Request<()>;

pub(crate) fn ifrange_match(
    hdr: &davheaders::IfRange,
    tag: &typed_headers::EntityTag,
    date: SystemTime,
) -> bool
{
    match hdr {
        &davheaders::IfRange::Date(ref d) => typed_headers::HttpDate::from(date) < *d,
        &davheaders::IfRange::EntityTag(ref t) => t == tag,
    }
}

pub(crate) fn etaglist_match(tags: &davheaders::ETagList, tag: &typed_headers::EntityTag) -> bool {
    match tags {
        &davheaders::ETagList::Star => true,
        &davheaders::ETagList::Tags(ref t) => t.iter().any(|x| x == tag),
    }
}

// Handle the if-headers: RFC 7232, HTTP/1.1 Conditional Requests.
pub(crate) fn http_if_match(req: &Request, meta: Option<&Box<DavMetaData>>) -> Option<StatusCode> {
    let modified = meta.and_then(|m| m.modified().ok());

    if let Some(r) = req.headers().typed_get::<davheaders::IfMatch>() {
        let etag = meta.map(|m| EntityTag::new(false, m.etag()));
        if etag.map_or(true, |m| !etaglist_match(&r.0, &m)) {
            debug!("precondition fail: If-Match {:?}", r);
            return Some(StatusCode::PRECONDITION_FAILED);
        }
    } else if let Some(r) = req.headers().typed_get::<typed_headers::IfUnmodifiedSince>() {
        match modified {
            None => return Some(StatusCode::PRECONDITION_FAILED),
            Some(m) => {
                if typed_headers::HttpDate::from(m) > r.0 {
                    debug!("precondition fail: If-Unmodified-Since {:?}", r.0);
                    return Some(StatusCode::PRECONDITION_FAILED);
                }
            },
        }
    }

    if let Some(r) = req.headers().typed_get::<davheaders::IfNoneMatch>() {
        let etag = meta.map(|m| EntityTag::new(false, m.etag()));
        if etag.map_or(false, |m| etaglist_match(&r.0, &m)) {
            debug!("precondition fail: If-None-Match {:?}", r);
            if req.method() == &Method::GET || req.method() == &Method::HEAD {
                return Some(StatusCode::NOT_MODIFIED);
            } else {
                return Some(StatusCode::PRECONDITION_FAILED);
            }
        }
    } else if let Some(r) = req.headers().typed_get::<typed_headers::IfModifiedSince>() {
        if req.method() == &Method::GET || req.method() == &Method::HEAD {
            if let Some(m) = modified {
                if typed_headers::HttpDate::from(m) > r.0 {
                    debug!("not-modified If-Modified-Since {:?}", r.0);
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
pub(crate) async fn dav_if_match<'a>(
    req: &'a Request,
    fs: &'a Box<DavFileSystem + 'static>,
    ls: &'a Option<Box<DavLockSystem + 'static>>,
    path: &'a WebPath,
) -> (bool, Vec<String>)
{
    let mut tokens: Vec<String> = Vec::new();
    let mut any_list_ok = false;

    let r = match req.headers().typed_get::<davheaders::If>() {
        Some(r) => r,
        None => return (true, tokens),
    };

    for iflist in r.0.iter() {
        // save and return all statetokens that we encountered.
        let toks = iflist.conditions.iter().filter_map(|c| {
            match &c.item {
                &davheaders::IfItem::StateToken(ref t) => Some(t.to_owned()),
                _ => None,
            }
        });
        tokens.extend(toks);

        // skip over if a previous list already evaluated to true.
        if any_list_ok == true {
            continue;
        }

        // find the resource that this list is about.
        let mut pa: Option<WebPath> = None;
        let (p, valid) = match iflist.resource_tag {
            Some(ref url) => {
                match WebPath::from_url(url, std::str::from_utf8(&path.prefix).unwrap()) {
                    Ok(p) => {
                        // anchor webpath in pa.
                        let p: &WebPath = pa.get_or_insert(p);
                        (p, true)
                    },
                    Err(_) => (path, false),
                }
            },
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
                        match ls {
                            &Some(ref ls) => ls.check(p, None, true, false, vec![s]).is_ok(),
                            &None => false,
                        }
                    }
                },
                davheaders::IfItem::ETag(ref tag) => {
                    if !valid {
                        // invalid location, so always false.
                        false
                    } else {
                        match await!(fs.metadata(p)) {
                            Ok(meta) => {
                                // exists and has metadata, now match.
                                tag == &EntityTag::new(false, meta.etag())
                            },
                            Err(_) => {
                                // metadata error, fail.
                                false
                            },
                        }
                    }
                },
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
        debug!("precondition fail: If {:?}", r.0);
    }
    (any_list_ok, tokens)
}

// Handle both the HTTP conditional If: headers, and the webdav If: header.
pub(crate) async fn if_match<'a>(
    req: &'a Request,
    meta: Option<&'a Box<DavMetaData + 'static>>,
    fs: &'a Box<DavFileSystem + 'static>,
    ls: &'a Option<Box<DavLockSystem + 'static>>,
    path: &'a WebPath,
) -> Option<StatusCode>
{
    match await!(dav_if_match(req, fs, ls, path)) {
        (true, _) => {},
        (false, _) => return Some(StatusCode::PRECONDITION_FAILED),
    }
    http_if_match(req, meta)
}

// Like if_match, but also returns all "associated state-tokens"
pub(crate) async fn if_match_get_tokens<'a>(
    req: &'a Request,
    meta: Option<&'a Box<DavMetaData + 'static>>,
    fs: &'a Box<DavFileSystem + 'static>,
    ls: &'a Option<Box<DavLockSystem + 'static>>,
    path: &'a WebPath,
) -> Result<Vec<String>, StatusCode>
{
    if let Some(code) = http_if_match(req, meta) {
        return Err(code);
    }
    match await!(dav_if_match(req, fs, ls, path)) {
        (true, v) => Ok(v),
        (false, _) => Err(StatusCode::PRECONDITION_FAILED),
    }
}
