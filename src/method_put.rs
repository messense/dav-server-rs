
use hyper;
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use std;
use std::io::prelude::*;

use DavResult;
use fs::*;
use headers;
use {statuserror,fserror,systemtime_to_httpdate};
use conditional::if_match_get_tokens;

macro_rules! statuserror {
    ($res:ident, $s:ident) => {
        return Err(statuserror(&mut $res, SC::$s))?
    }
}

const SABRE: &'static str = "application/x-sabredav-partialupdate";

impl super::DavHandler {

    pub(crate) fn handle_put(&self, mut req: Request, mut res: Response) -> DavResult<()> {

        let mut start = 0;
        let mut count = 0;
        let mut have_count = false;
        let mut do_range = false;

        let mut oo = OpenOptions::write();
        oo.create = true;
        oo.truncate = true;

        if let Some(n) = req.headers.get::<hyper::header::ContentLength>() {
            count = n.0;
            have_count = true;
        }
        let path = self.path(&req);
        let meta = self.fs.metadata(&path);

        // close connection on error.
        res.headers_mut().set(hyper::header::Connection::close());

        // SabreDAV style PATCH?
        if req.method == hyper::method::Method::Patch {
            if !req.headers.get::<headers::ContentType>()
                    .map_or(false, |ct| ct.0 == SABRE) {
                //return Err(statuserror(&mut res, SC::UnsupportedMediaType));
                statuserror!(res, UnsupportedMediaType);
            }
            if !have_count {
                return Err(statuserror(&mut res, SC::LengthRequired));
            };
            let r = req.headers.get::<headers::XUpdateRange>()
                .ok_or(statuserror(&mut res, SC::BadRequest))?;
            match r {
                &headers::XUpdateRange::FromTo(b, e) => {
                    if e - b + 1 != count {
                        *res.status_mut() = SC::RangeNotSatisfiable;
                        return Ok(());
                    }
                    start = b;
                },
                &headers::XUpdateRange::AllFrom(b) => {
                    start = b;
                },
                &headers::XUpdateRange::Last(n) => {
                    if let Ok(ref m) = meta {
                        if n > m.len() {
                            return Err(statuserror(&mut res, SC::RangeNotSatisfiable));
                        }
                        start = m.len() - n;
                    }
                },
                &headers::XUpdateRange::Append => {
                    oo.append = true;
                }
            }
            do_range = true;
            oo.truncate = false;
        }

        // Apache-style Content-Range header?
        if let Some(x) = req.headers.get::<headers::ContentRange>() {

            let b = x.0;
            let e = x.1;

            if have_count {
                if e - b + 1 != count {
                    return Err(statuserror(&mut res, SC::RangeNotSatisfiable));
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
        let tokens = match if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &path) {
            Ok(t) => t,
            Err(s) => return Err(statuserror(&mut res, s)),
        };

        // XXX FIXME multistatus error
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            if let Err(_l) = locksystem.check(&path, false, t) {
                return Err(statuserror(&mut res, SC::Locked));
            }
        }

        // tweak open options.
        if req.headers.get::<headers::IfMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create_new = true;
        }
        if req.headers.get::<headers::IfNoneMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create = false;
        }

        let mut file = match self.fs.open(&path, oo) {
            Ok(f) => f,
            Err(FsError::NotFound) |
            Err(FsError::Exists) => {
                let s = if !oo.create || oo.create_new {
                    SC::PreconditionFailed
                } else {
                    SC::Conflict
                };
                return Err(statuserror(&mut res, s))
            },
            Err(e) => return Err(fserror(&mut res, e)),
        };

        if do_range {
            // seek to beginning of requested data.
            if let Err(_) = file.seek(std::io::SeekFrom::Start(start)) {
                return Err(statuserror(&mut res, SC::RangeNotSatisfiable));
            }
        }

        let bytes = vec![hyper::header::RangeUnit::Bytes];
        res.headers_mut().set(hyper::header::AcceptRanges(bytes));

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
            return Err(statuserror(&mut res, SC::BadRequest));
        }

        // Report whether we created or updated the file.
        *res.status_mut() = match meta {
            Ok(_) => SC::NoContent,
            Err(_) => {
                res.headers_mut().set(hyper::header::ContentLength(0));
                SC::Created
            },
        };

        // no errors, connection may be kept open.
        res.headers_mut().remove::<hyper::header::Connection>();

        if let Ok(m) = file.metadata() {
            let file_etag = hyper::header::EntityTag::new(false, m.etag());
            res.headers_mut().set(hyper::header::ETag(file_etag));

            if let Ok(modified) = m.modified() {
                res.headers_mut().set(hyper::header::LastModified(
                        systemtime_to_httpdate(modified)));
            }
        }
        Ok(())
    }
}

