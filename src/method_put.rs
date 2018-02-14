
use hyper;
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use std;
use std::io::prelude::*;

use super::DavResult;
use super::fs::*;
use super::headers;
use super::{statuserror,fserror,systemtime_to_httpdate};
use super::conditional::ifmatch;

macro_rules! statuserror {
    ($res:ident, $s:ident) => {
        return Err(statuserror(&mut $res, SC::$s))?
    }
}

const SABRE: &'static str = "application/x-sabredav-partialupdate";

impl super::DavHandler {

    pub(crate) fn handle_put(&self, mut req: Request, res: Response) -> DavResult<()> {

        // handle the PUT request, then drain the input if there's still
        // data coming in (usually if we rejected the request). There really
        // should be a way to forcibly close the network connection instead.
        let result = self.do_handle_put(&mut req, res);
        self.drain_request(&mut req);
        result
    }

    fn do_handle_put(&self, req: &mut Request, mut res: Response) -> DavResult<()> {

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

        // handle the if-headers.
        if let Some(s) = ifmatch(&req, meta.as_ref().ok()) {
            return Err(statuserror(&mut res, s));
        }

        if req.headers.get::<headers::IfMatch>().map_or(false, |h| h.is_star()) {
            oo.create_new = true;
        }
        if req.headers.get::<headers::IfNoneMatch>().map_or(false, |h| h.is_star()) {
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

        // Return updated file information.
        *res.status_mut() = match meta {
            Ok(_) => SC::NoContent,
            Err(_) => SC::Created,
        };
        res.headers_mut().set(hyper::header::ContentLength(0));

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

