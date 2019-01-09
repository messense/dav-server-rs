use std::io::prelude::*;

use http::StatusCode as SC;

use crate::sync_adapter::{Request,Response};
use crate::typed_headers::{self, HeaderMapExt};
use crate::DavResult;
use crate::fs::*;
use crate::headers;
use crate::{statuserror,fserror,systemtime_to_httpdate};
use crate::conditional::if_match_get_tokens;

const SABRE: &'static str = "application/x-sabredav-partialupdate";

impl crate::DavInner {

    pub(crate) fn handle_put(&self, mut req: Request, mut res: Response) -> DavResult<()> {

        let mut start = 0;
        let mut count = 0;
        let mut have_count = false;
        let mut do_range = false;

        let mut oo = OpenOptions::write();
        oo.create = true;
        oo.truncate = true;

        if let Some(n) = req.headers.typed_get::<typed_headers::ContentLength>() {
            count = n.0;
            have_count = true;
        }
        let path = self.path(&req);
        let meta = self.fs.metadata(&path);

        // close connection on error.
        res.headers_mut().typed_insert(typed_headers::Connection::close());

        // SabreDAV style PATCH?
        if req.method == http::Method::PATCH {
            if !req.headers.typed_get::<headers::ContentType>()
                    .map_or(false, |ct| ct.0 == SABRE) {
                return Err(statuserror(&mut res, SC::UNSUPPORTED_MEDIA_TYPE));
            }
            if !have_count {
                return Err(statuserror(&mut res, SC::LENGTH_REQUIRED));
            };
            let r = req.headers.typed_get::<headers::XUpdateRange>()
                .ok_or(statuserror(&mut res, SC::BAD_REQUEST))?;
            match r {
                headers::XUpdateRange::FromTo(b, e) => {
                    if e - b + 1 != count {
                        *res.status_mut() = SC::RANGE_NOT_SATISFIABLE;
                        return Ok(());
                    }
                    start = b;
                },
                headers::XUpdateRange::AllFrom(b) => {
                    start = b;
                },
                headers::XUpdateRange::Last(n) => {
                    if let Ok(ref m) = meta {
                        if n > m.len() {
                            return Err(statuserror(&mut res, SC::RANGE_NOT_SATISFIABLE));
                        }
                        start = m.len() - n;
                    }
                },
                headers::XUpdateRange::Append => {
                    oo.append = true;
                }
            }
            do_range = true;
            oo.truncate = false;
        }

        // Apache-style Content-Range header?
        if let Some(x) = req.headers.typed_get::<headers::ContentRange>() {

            let b = x.0;
            let e = x.1;

            if have_count {
                if e - b + 1 != count {
                    return Err(statuserror(&mut res, SC::RANGE_NOT_SATISFIABLE));
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
        let tokens = match if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path) {
            Ok(t) => t,
            Err(s) => return Err(statuserror(&mut res, s)),
        };

        // if locked check if we hold that lock.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if let Err(_l) = locksystem.check(&path, principal, false, false, t) {
                return Err(statuserror(&mut res, SC::LOCKED));
            }
        }

        // tweak open options.
        if req.headers.typed_get::<headers::IfMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create = false;
        }
        if req.headers.typed_get::<headers::IfNoneMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create_new = true;
        }

        let mut file = match self.fs.open(&path, oo) {
            Ok(f) => f,
            Err(FsError::NotFound) |
            Err(FsError::Exists) => {
                let s = if !oo.create || oo.create_new {
                    SC::PRECONDITION_FAILED
                } else {
                    SC::CONFLICT
                };
                return Err(statuserror(&mut res, s))
            },
            Err(e) => return Err(fserror(&mut res, e)),
        };

        if do_range {
            // seek to beginning of requested data.
            if let Err(_) = file.seek(std::io::SeekFrom::Start(start)) {
                return Err(statuserror(&mut res, SC::RANGE_NOT_SATISFIABLE));
            }
        }

        let bytes = vec![typed_headers::RangeUnit::Bytes];
        res.headers_mut().typed_insert(typed_headers::AcceptRanges(bytes));

        // loop, read body, write to file.
        let mut buffer = [0; 8192];
        let mut bad = false;
        loop {
            let mut n = req.read(&mut buffer[..])?;
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
            let data = &buffer[..n];
            file.write_all(data)?;
        }
        file.flush()?;
        if bad {
            return Err(statuserror(&mut res, SC::BAD_REQUEST));
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

        if let Ok(m) = file.metadata() {
            let file_etag = typed_headers::EntityTag::new(false, m.etag());
            res.headers_mut().typed_insert(typed_headers::ETag(file_etag));

            if let Ok(modified) = m.modified() {
                res.headers_mut().typed_insert(typed_headers::LastModified(
                        systemtime_to_httpdate(modified)));
            }
        }
        Ok(())
    }
}

