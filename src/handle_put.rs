use std::any::Any;
use std::error::Error as StdError;
use std::io;

use http::StatusCode as SC;
use http::{self, Request, Response};

use crate::common::*;
use crate::conditional::if_match_get_tokens;
use crate::fs::*;
use crate::headers;
use crate::typed_headers::{self, HeaderMapExt};
use crate::{empty_body, systemtime_to_httpdate};
use crate::{BoxedByteStream, DavError, DavResult};

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
    pub(crate) fn handle_put<ReqBody, ReqError>(
        self,
        req: Request<()>,
        body: ReqBody,
    ) -> impl Future03<Output = DavResult<Response<BoxedByteStream>>>
    where
        ReqBody: Stream<Item = bytes::Bytes, Error = ReqError> + 'static,
        ReqError: StdError + Sync + Send + 'static,
    {
        async move {
            let mut start = 0;
            let mut count = 0;
            let mut have_count = false;
            let mut do_range = false;

            let mut oo = OpenOptions::write();
            oo.create = true;
            oo.truncate = true;

            if let Some(n) = req.headers().typed_get::<typed_headers::ContentLength>() {
                count = n.0;
                have_count = true;
            }
            let path = self.path(&req);
            let meta = await!(self.fs.metadata(&path));

            // close connection on error.
            let mut res = Response::new(empty_body());
            res.headers_mut().typed_insert(typed_headers::Connection::close());

            // SabreDAV style PATCH?
            if req.method() == &http::Method::PATCH {
                if !req
                    .headers()
                    .typed_get::<headers::ContentType>()
                    .map_or(false, |ct| ct.0 == SABRE)
                {
                    return Err(DavError::StatusClose(SC::UNSUPPORTED_MEDIA_TYPE));
                }
                if !have_count {
                    return Err(DavError::StatusClose(SC::LENGTH_REQUIRED));
                };
                let r = req
                    .headers()
                    .typed_get::<headers::XUpdateRange>()
                    .ok_or(DavError::StatusClose(SC::BAD_REQUEST))?;
                match r {
                    headers::XUpdateRange::FromTo(b, e) => {
                        if e - b + 1 != count {
                            return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                        }
                        start = b;
                    },
                    headers::XUpdateRange::AllFrom(b) => {
                        start = b;
                    },
                    headers::XUpdateRange::Last(n) => {
                        if let Ok(ref m) = meta {
                            if n > m.len() {
                                return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                            }
                            start = m.len() - n;
                        }
                    },
                    headers::XUpdateRange::Append => {
                        oo.append = true;
                    },
                }
                do_range = true;
                oo.truncate = false;
            }

            // Apache-style Content-Range header?
            if let Some(x) = req.headers().typed_get::<headers::ContentRange>() {
                let b = x.0;
                let e = x.1;

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

            // check the If and If-* headers.
            let tokens = if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path);
            let tokens = match await!(tokens) {
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
                .typed_get::<headers::IfMatch>()
                .map_or(false, |h| &h.0 == &headers::ETagList::Star)
            {
                oo.create = false;
            }
            if req
                .headers()
                .typed_get::<headers::IfNoneMatch>()
                .map_or(false, |h| &h.0 == &headers::ETagList::Star)
            {
                oo.create_new = true;
            }

            let mut file = match await!(self.fs.open(&path, oo)) {
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
                if let Err(_) = await!(file.seek(std::io::SeekFrom::Start(start))) {
                    return Err(DavError::StatusClose(SC::RANGE_NOT_SATISFIABLE));
                }
            }

            let bytes = vec![typed_headers::RangeUnit::Bytes];
            res.headers_mut().typed_insert(typed_headers::AcceptRanges(bytes));


            // loop, read body, write to file.
            let mut bad = false;

            // turn body stream into a futures@0.3 stream.
            let mut body = body.compat();

            while let Some(buffer) = await!(body.next()) {
                let buffer = buffer.map_err(|e| to_ioerror(e))?;
                let mut n = buffer.len();
                if have_count {
                    // bunch of consistency checks.
                    if n > 0 && count == 0 {
                        error!("PUT file: sender is sending more than promised");
                        bad = true;
                        break;
                    }
                    if n > count as usize {
                        n = count as usize;
                    }
                    if n == 0 && count > 0 {
                        error!("PUT file: premature EOF on input");
                        bad = true;
                    }
                    count -= n as u64;
                }
                if n == 0 {
                    break;
                }
                await!(file.write_all(&buffer))?;
            }
            await!(file.flush())?;
            if bad {
                return Err(DavError::StatusClose(SC::BAD_REQUEST));
            }

            // Report whether we created or updated the file.
            *res.status_mut() = match meta {
                Ok(_) => SC::NO_CONTENT,
                Err(_) => {
                    res.headers_mut().typed_insert(typed_headers::ContentLength(0));
                    SC::CREATED
                },
            };

            // no errors, connection may be kept open.
            res.headers_mut().remove(http::header::CONNECTION);

            if let Ok(m) = await!(file.metadata()) {
                let file_etag = typed_headers::EntityTag::new(false, m.etag());
                res.headers_mut().typed_insert(typed_headers::ETag(file_etag));

                if let Ok(modified) = m.modified() {
                    res.headers_mut()
                        .typed_insert(typed_headers::LastModified(systemtime_to_httpdate(modified)));
                }
            }
            Ok(res)
        }
    }
}
