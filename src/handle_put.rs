use std::any::Any;
use std::error::Error as StdError;
use std::io;

use bytes::{Buf, Bytes};
use headers::HeaderMapExt;
use http::StatusCode as SC;
use http::{self, Request, Response};
use http_body::Body as HttpBody;

use crate::body::Body;
use crate::conditional::if_match_get_tokens;
use crate::davheaders;
use crate::fs::*;
use crate::{DavError, DavResult};

const SABRE: &'static str = "application/x-sabredav-partialupdate";

// This is a nice hack. If the type 'E' is actually an io::Error or a Box<io::Error>,
// convert it back into a real io::Error. If it is a DavError or a Box<DavError>,
// use its Into<io::Error> impl. Otherwise just wrap the error in io::Error::new.
//
// If we had specialization this would look a lot prettier.
//
// Also, this is senseless. It's not as if we _do_ anything with the
// io::Error, other than noticing "oops an error occured".
fn to_ioerror<E>(err: E) -> io::Error
where E: StdError + Sync + Send + 'static {
    let e = &err as &dyn Any;
    if e.is::<io::Error>() || e.is::<Box<io::Error>>() {
        let err = Box::new(err) as Box<dyn Any>;
        match err.downcast::<io::Error>() {
            Ok(e) => *e,
            Err(e) => {
                match e.downcast::<Box<io::Error>>() {
                    Ok(e) => *(*e),
                    Err(_) => io::ErrorKind::Other.into(),
                }
            },
        }
    } else if e.is::<DavError>() || e.is::<Box<DavError>>() {
        let err = Box::new(err) as Box<dyn Any>;
        match err.downcast::<DavError>() {
            Ok(e) => (*e).into(),
            Err(e) => {
                match e.downcast::<Box<DavError>>() {
                    Ok(e) => (*(*e)).into(),
                    Err(_) => io::ErrorKind::Other.into(),
                }
            },
        }
    } else {
        io::Error::new(io::ErrorKind::Other, err)
    }
}

impl crate::DavInner {
    pub(crate) async fn handle_put<ReqBody, ReqData, ReqError>(
        self,
        req: &Request<()>,
        body: ReqBody,
    ) -> DavResult<Response<Body>>
    where
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let mut start = 0;
        let mut count = 0;
        let mut have_count = false;
        let mut do_range = false;

        let mut oo = OpenOptions::write();
        oo.create = true;
        oo.truncate = true;

        if let Some(n) = req.headers().typed_get::<headers::ContentLength>() {
            count = n.0;
            have_count = true;
            oo.size = Some(count);
        } else if let Some(n) = req.headers()
                                   .get("X-Expected-Entity-Length")
                                   .and_then(|v| v.to_str().ok()) {
            // macOS Finder, see https://evertpot.com/260/
            if let Ok(len) = n.parse() {
                count = len;
                have_count = true;
                oo.size = Some(count);
            }
        }
        let path = self.path(&req);
        let meta = self.fs.metadata(&path).await;

        // close connection on error.
        let mut res = Response::new(Body::empty());
        res.headers_mut().typed_insert(headers::Connection::close());

        // SabreDAV style PATCH?
        if req.method() == &http::Method::PATCH {
            if !req
                .headers()
                .typed_get::<davheaders::ContentType>()
                .map_or(false, |ct| ct.0 == SABRE)
            {
                return Err(DavError::StatusClose(SC::UNSUPPORTED_MEDIA_TYPE));
            }
            if !have_count {
                return Err(DavError::StatusClose(SC::LENGTH_REQUIRED));
            };
            let r = req
                .headers()
                .typed_get::<davheaders::XUpdateRange>()
                .ok_or(DavError::StatusClose(SC::BAD_REQUEST))?;
            match r {
                davheaders::XUpdateRange::FromTo(b, e) => {
                    if b > e || e - b + 1 != count {
                        return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                    }
                    start = b;
                },
                davheaders::XUpdateRange::AllFrom(b) => {
                    start = b;
                },
                davheaders::XUpdateRange::Last(n) => {
                    if let Ok(ref m) = meta {
                        if n > m.len() {
                            return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                        }
                        start = m.len() - n;
                    }
                },
                davheaders::XUpdateRange::Append => {
                    oo.append = true;
                },
            }
            do_range = true;
            oo.truncate = false;
        }

        // Apache-style Content-Range header?
        match req.headers().typed_try_get::<headers::ContentRange>() {
            Ok(Some(range)) => {
                if let Some((b, e)) = range.bytes_range() {
                    if b > e {
                        return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                    }

                    if have_count {
                        if e - b + 1 != count {
                            return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                        }
                    } else {
                        count = e - b + 1;
                        have_count = true;
                    }
                    start = b;
                    do_range = true;
                    oo.truncate = false;
                }
            },
            Ok(None) => {},
            Err(_) => return Err(DavError::StatusClose(SC::BAD_REQUEST)),
        }

        // check the If and If-* headers.
        let tokens = if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path);
        let tokens = match tokens.await {
            Ok(t) => t,
            Err(s) => return Err(DavError::StatusClose(s)),
        };

        // if locked check if we hold that lock.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if let Err(_l) = locksystem.check(&path, principal, false, false, t) {
                return Err(DavError::StatusClose(SC::LOCKED));
            }
        }

        // tweak open options.
        if req
            .headers()
            .typed_get::<davheaders::IfMatch>()
            .map_or(false, |h| &h.0 == &davheaders::ETagList::Star)
        {
            oo.create = false;
        }
        if req
            .headers()
            .typed_get::<davheaders::IfNoneMatch>()
            .map_or(false, |h| &h.0 == &davheaders::ETagList::Star)
        {
            oo.create_new = true;
        }

        let mut file = match self.fs.open(&path, oo).await {
            Ok(f) => f,
            Err(FsError::NotFound) | Err(FsError::Exists) => {
                let s = if !oo.create || oo.create_new {
                    SC::PRECONDITION_FAILED
                } else {
                    SC::CONFLICT
                };
                return Err(DavError::StatusClose(s));
            },
            Err(e) => return Err(DavError::FsError(e)),
        };

        if do_range {
            // seek to beginning of requested data.
            if let Err(_) = file.seek(std::io::SeekFrom::Start(start)).await {
                return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
            }
        }

        res.headers_mut().typed_insert(headers::AcceptRanges::bytes());

        pin_utils::pin_mut!(body);

        // loop, read body, write to file.
        let mut total = 0u64;

        while let Some(data) = body.data().await {
            let mut buf = data.map_err(|e| to_ioerror(e))?;
            let buflen = buf.remaining();
            total += buflen as u64;
            // consistency check.
            if have_count && total > count {
                break;
            }
            // The `Buf` might actually be a `Bytes`.
            let b = {
                let b: &mut dyn std::any::Any = &mut buf;
                b.downcast_mut::<Bytes>()
            };
            if let Some(bytes) = b {
                let bytes = std::mem::replace(bytes, Bytes::new());
                file.write_bytes(bytes).await?;
            } else {
                file.write_buf(Box::new(buf)).await?;
            }
        }
        file.flush().await?;

        if have_count && total > count {
            error!("PUT file: sender is sending more bytes than expected");
            return Err(DavError::StatusClose(SC::BAD_REQUEST));
        }

        if have_count && total < count {
            error!("PUT file: premature EOF on input");
            return Err(DavError::StatusClose(SC::BAD_REQUEST));
        }

        // Report whether we created or updated the file.
        *res.status_mut() = match meta {
            Ok(_) => SC::NO_CONTENT,
            Err(_) => {
                res.headers_mut().typed_insert(headers::ContentLength(0));
                SC::CREATED
            },
        };

        // no errors, connection may be kept open.
        res.headers_mut().remove(http::header::CONNECTION);

        if let Ok(m) = file.metadata().await {
            if let Some(etag) = davheaders::ETag::from_meta(&m) {
                res.headers_mut().typed_insert(etag);
            }
            if let Ok(modified) = m.modified() {
                res.headers_mut()
                    .typed_insert(headers::LastModified::from(modified));
            }
        }
        Ok(res)
    }
}
