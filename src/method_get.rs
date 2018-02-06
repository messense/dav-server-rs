
use hyper;
use hyper::server::{Request,Response};
use hyper::status::StatusCode;
use hyper::header::ByteRangeSpec;

use std;
use std::io::prelude::*;

use super::errors::DavError;
use super::headers;
use super::fs::OpenOptions;
use super::{fserror,statuserror,systemtime_to_httpdate};
use super::conditional::ifmatch;

impl super::DavHandler {
    pub(crate) fn handle_get(&self, req: Request, mut res: Response) -> Result<(), DavError> {
        let head = req.method == hyper::method::Method::Head;

        // open file and get metadata.
        let mut file = self.fs.open(&self.path(&req), OpenOptions::read())
            .map_err(|e| fserror(&mut res, e))?;
        let meta = file.metadata();
        if !meta.is_file() {
            return Err(statuserror(&mut res, StatusCode::MethodNotAllowed));
        }

        let mut start = 0;
        let mut count = meta.len();
        let len = count;
        let mut do_range = true;

        let file_etag = hyper::header::EntityTag::new(false, meta.etag());

        if let Some(r) = req.headers.get::<headers::IfRange>() {
            do_range = r.matches(&file_etag, meta.modified().unwrap());
        }

        // see if we want to get a range.
        if do_range {
            do_range = false;
            if let Some(r) = req.headers.get::<hyper::header::Range>() {
                if let &hyper::header::Range::Bytes(ref ranges) = r {
                    // we only support a single range
                    if ranges.len() == 1 {
                        match &ranges[0] {
                            &ByteRangeSpec::FromTo(s, e) => {
                                start = s; count = e - s + 1;
                            },
                            &ByteRangeSpec::AllFrom(s) => {
                                start = s; count = len - s;
                            },
                            &ByteRangeSpec::Last(n) => {
                                start = len - n; count = n;
                            },
                        }
                        if start >= len {
                            return Err(statuserror(&mut res, StatusCode::RangeNotSatisfiable));
                        }
                        if start + count > len {
                            count = len - start;
                        }
                        do_range = true;
                    }
                }
            }
        }

        // set Last-Modified and ETag headers.
        if let Ok(modified) = meta.modified() {
            res.headers_mut().set(hyper::header::LastModified(
                    systemtime_to_httpdate(modified)));
        }
        res.headers_mut().set(hyper::header::ETag(file_etag));

        // handle the if-headers.
        if let Some(s) = ifmatch(&req, Some(&meta)) {
            return Err(statuserror(&mut res, s));
        }

        if do_range {
            // seek to beginning of requested data.
            if let Err(_) = file.seek(std::io::SeekFrom::Start(start)) {
                *res.status_mut() = StatusCode::RangeNotSatisfiable;
                return Ok(());
            }

            // set partial-content status and add content-range header.
            let r = format!("bytes {}-{}/{}", start, start + count - 1, len);
            res.headers_mut().set_raw("Content-Range",
                                    vec!(r.as_bytes().to_vec()));
            *res.status_mut() = StatusCode::PartialContent;
        } else {
            // normal request, send entire file.
            *res.status_mut() = StatusCode::Ok;
        }

        // set content-length and start.
        res.headers_mut().set(hyper::header::ContentLength(count));
        res.headers_mut().set(hyper::header::AcceptRanges(vec![hyper::header::RangeUnit::Bytes]));

        if head {
            return Ok(())
        }

        // now just loop and send data.
        let mut writer = res.start()?;

        let mut buffer = [0; 8192];
        let zero = [0; 4096];

        while count > 0 {
            let data;
            let mut n = file.read(&mut buffer[..])?;
            if n > count as usize {
                n = count as usize;
            }
            if n == 0 {
                // this is a cop out. if the file got truncated, just
                // return zero bytes instead of file content.
                n = if count > 4096 { 4096 } else { count as usize };
                data = &zero[..n];
            } else {
                data = &buffer[..n];
            }
            count -= n as u64;
            writer.write_all(data)?;
        }
        Ok(())
    }
}

