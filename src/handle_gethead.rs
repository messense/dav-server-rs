use std::cmp;
use std::io::Write;

use futures::{Future, StreamExt};
use htmlescape;
use http::{status::StatusCode, Request, Response};
use time;

use bytes::Bytes;

use crate::conditional;
use crate::corostream::CoroStream;
use crate::davheaders;
use crate::errors::*;
use crate::fs::*;
use crate::typed_headers::{self, ByteRangeSpec, HeaderMapExt};
use crate::util::{empty_body, systemtime_to_timespec};
use crate::{BoxedByteStream, Method};

struct Range {
    start: u64,
    count: u64,
}

const BOUNDARY: &str = "BOUNDARY";
const BOUNDARY_START: &str = "\n--BOUNDARY\n";
const BOUNDARY_END: &str = "\n--BOUNDARY--\n";

const READ_BUF_SIZE: usize = 16384;

impl crate::DavInner {
    pub(crate) fn handle_get(
        self,
        req: Request<()>,
    ) -> impl Future<Output = Result<Response<BoxedByteStream>, DavError>>
    {
        let path = self.path(&req);

        async move {
            // check if it's a directory.
            let head = req.method() == &http::Method::HEAD;
            let meta = await!(self.fs.metadata(&path))?;
            if meta.is_dir() {
                return await!(self.handle_dirlist(req, head));
            }

            // double check, is it a regular file.
            let mut file = await!(self.fs.open(&path, OpenOptions::read()))?;
            let meta = await!(file.metadata())?;
            if !meta.is_file() {
                return Err(DavError::Status(StatusCode::METHOD_NOT_ALLOWED));
            }
            let len = meta.len();
            let mut curpos = 0u64;
            let file_etag = meta.etag().map(|tag| typed_headers::EntityTag::new(false, tag));

            let mut ranges = Vec::new();
            let do_range = match req.headers().typed_get::<davheaders::IfRange>() {
                Some(r) => conditional::ifrange_match(&r, file_etag.as_ref(), meta.modified().ok()),
                None => true,
            };

            // see if we want to get one or more ranges.
            if do_range {
                if let Some(r) = req.headers().typed_get::<typed_headers::Range>() {
                    debug!("handle_gethead: range header {:?}", r);
                    match r {
                        typed_headers::Range::Bytes(ref byteranges) => {
                            for range in byteranges {
                                let (start, mut count) = match range {
                                    &ByteRangeSpec::FromTo(s, e) => (s, e - s + 1),
                                    &ByteRangeSpec::AllFrom(s) => (s, len - s),
                                    &ByteRangeSpec::Last(n) => (len - n, n),
                                };
                                if start >= len {
                                    return Err(DavError::Status(StatusCode::RANGE_NOT_SATISFIABLE));
                                }
                                if start + count > len {
                                    count = len - start;
                                }
                                ranges.push(Range { start, count });
                            }
                        },
                        _ => {},
                    }
                }
            }

            let mut res = Response::new(empty_body());

            // set Last-Modified and ETag headers.
            if let Ok(modified) = meta.modified() {
                res.headers_mut()
                    .typed_insert(typed_headers::LastModified(modified.into()));
            }
            if let Some(etag) = file_etag {
                res.headers_mut().typed_insert(typed_headers::ETag(etag));
            }

            // handle the if-headers.
            if let Some(s) = await!(conditional::if_match(
                &req,
                Some(&meta),
                &self.fs,
                &self.ls,
                &path
            )) {
                return Err(DavError::Status(s));
            }

            if ranges.len() > 0 {
                // seek to beginning of the first range.
                if let Err(_) = await!(file.seek(std::io::SeekFrom::Start(ranges[0].start))) {
                    let r = format!("bytes */{}", len);
                    res.headers_mut().insert("Content-Range", r.parse().unwrap());
                    *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                    return Ok(res);
                }
                curpos = ranges[0].start;

                *res.status_mut() = StatusCode::PARTIAL_CONTENT;
                if ranges.len() == 1 {
                    // add content-range header.
                    let r = format!(
                        "bytes {}-{}/{}",
                        ranges[0].start,
                        ranges[0].start + ranges[0].count - 1,
                        len
                    );
                    res.headers_mut().insert("Content-Range", r.parse().unwrap());
                } else {
                    // add content-type header.
                    let r = format!("multipart/byteranges; boundary={}", BOUNDARY);
                    res.headers_mut().insert("Content-Type", r.parse().unwrap());
                }
            } else {
                // normal request, send entire file.
                *res.status_mut() = StatusCode::OK;
                ranges.push(Range { start: 0, count: len });
            }

            // Apache always adds an Accept-Ranges header, even with partial
            // responses where it should be pretty obvious. So something somewhere
            // probably depends on that.
            res.headers_mut()
                .typed_insert(typed_headers::AcceptRanges(vec![typed_headers::RangeUnit::Bytes]));

            // set content-length and start if we're not doing multipart.
            let content_type = path.get_mime_type_str();
            if ranges.len() <= 1 {
                res.headers_mut()
                    .insert("Content-Type", content_type.parse().unwrap());
                res.headers_mut()
                    .typed_insert(typed_headers::ContentLength(ranges[0].count));
            }

            if head {
                return Ok(res);
            }

            // now just loop and send data.
            *res.body_mut() = Box::new(CoroStream::new(async move |mut tx| {
                let mut buffer = [0; READ_BUF_SIZE];
                let zero = [0; 4096];

                let multipart = ranges.len() > 1;
                for range in ranges {

                    debug!("handle_get: start = {}, count = {}", range.start, range.count);
                    if curpos != range.start {
                        // this should never fail, but if it does, just skip this range
                        // and try the next one.
                        if let Err(_e) = await!(file.seek(std::io::SeekFrom::Start(range.start))) {
                            debug!("handle_get: failed to seek to {}: {:?}", range.start, _e);
                            continue;
                        }
                        curpos = range.start;
                    }

                    if multipart {
                        let mut hdrs = Vec::new();
                        let _ = write!(hdrs, "{}", BOUNDARY_START);
                        let _ = writeln!(
                            hdrs,
                            "Content-Range: bytes {}-{}/{}",
                            range.start,
                            range.start + range.count - 1,
                            len
                        );
                        let _ = writeln!(hdrs, "Content-Type: {}", content_type);
                        let _ = writeln!(hdrs, "");
                        await!(tx.send(Bytes::from(hdrs)));
                    }

                    let mut count = range.count;
                    while count > 0 {
                        let data;
                        let blen = cmp::min(count, READ_BUF_SIZE as u64) as usize;
                        let mut n = await!(file.read_bytes(&mut buffer[..blen]))?;
                        if n == 0 {
                            // this is a cop out. if the file got truncated, just
                            // return zero bytes instead of file content.
                            n = if count > 4096 { 4096 } else { count as usize };
                            data = &zero[..n];
                        } else {
                            data = &buffer[..n];
                        }
                        count -= n as u64;
                        curpos += n as u64;
                        debug!("sending {} bytes", data.len());
                        await!(tx.send(Bytes::from(data)));
                    }
                }
                if multipart {
                    await!(tx.send(Bytes::from(BOUNDARY_END)));
                }
                Ok::<(), std::io::Error>(())
            }));

            Ok(res)
        }
    }

    pub(crate) fn handle_dirlist(
        self,
        req: Request<()>,
        head: bool,
    ) -> impl Future<Output = Result<Response<BoxedByteStream>, DavError>>
    {
        let path = self.path(&req);

        async move {
            let mut res = Response::new(empty_body());

            // This is a directory. If the path doesn't end in "/", send a redir.
            // Most webdav clients handle redirect really bad, but a client asking
            // for a directory index is usually a browser.
            if !path.is_collection() {
                let mut path = path.clone();
                path.add_slash();
                res.headers_mut()
                    .insert("Location", path.as_utf8_string_with_prefix().parse().unwrap());
                res.headers_mut().typed_insert(typed_headers::ContentLength(0));
                *res.status_mut() = StatusCode::FOUND;
                return Ok(res);
            }

            // If we do not allow PROPFIND, we don't allow directory indexes either.
            if let Some(ref a) = self.allow {
                if !a.allowed(Method::PropFind) {
                    debug!("method {} not allowed on request {}", req.method(), req.uri());
                    return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
                }
            }

            // read directory or bail.
            let mut entries = await!(self.fs.read_dir(&path, ReadDirMeta::Data))?;

            // start output
            res.headers_mut()
                .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
            *res.status_mut() = StatusCode::OK;
            if head {
                return Ok(res);
            }

            // now just loop and send data.
            *res.body_mut() = Box::new(CoroStream::new(async move |mut tx| {
                // transform all entries into a dirent struct.
                struct Dirent {
                    path: String,
                    name: String,
                    meta: Box<DavMetaData>,
                }

                let mut dirents: Vec<Dirent> = Vec::new();
                while let Some(dirent) = await!(entries.next()) {
                    let mut name = dirent.name();
                    if name.starts_with(b".") {
                        continue;
                    }
                    let mut npath = path.clone();
                    npath.push_segment(&name);
                    if let Ok(meta) = await!(dirent.metadata()) {
                        if meta.is_dir() {
                            name.push(b'/');
                            npath.add_slash();
                        }
                        dirents.push(Dirent {
                            path: npath.as_url_string_with_prefix(),
                            name: String::from_utf8_lossy(&name).to_string(),
                            meta: meta,
                        });
                    }
                }

                // now we can sort the dirent struct.
                dirents.sort_by(|a, b| {
                    let adir = a.meta.is_dir();
                    let bdir = b.meta.is_dir();
                    if adir && !bdir {
                        std::cmp::Ordering::Less
                    } else if bdir && !adir {
                        std::cmp::Ordering::Greater
                    } else {
                        (a.name).cmp(&b.name)
                    }
                });

                // and output html
                let upath = htmlescape::encode_minimal(&path.as_url_string_with_prefix());
                let mut w = String::new();
                w.push_str("<html><head>");
                w.push_str(&format!("<title>Index of {}</title>", upath));
                w.push_str("<style>");
                w.push_str("table {{");
                w.push_str("  border-collapse: separate;");
                w.push_str("  border-spacing: 1.5em 0.25em;");
                w.push_str("}}");
                w.push_str("h1 {{");
                w.push_str("  padding-left: 0.3em;");
                w.push_str("}}");
                w.push_str(".mono {{");
                w.push_str("  font-family: monospace;");
                w.push_str("}}");
                w.push_str("</style>");
                w.push_str("</head>");

                w.push_str("<body>");
                w.push_str(&format!("<h1>Index of {}</h1>", upath));
                w.push_str("<table>");
                w.push_str("<tr>");
                w.push_str("<th>Name</th><th>Last modified</th><th>Size</th>");
                w.push_str("<tr><th colspan=\"3\"><hr></th></tr>");
                w.push_str("<tr><td><a href=\"..\">Parent Directory</a></td><td>&nbsp;</td><td class=\"mono\" align=\"right\">[DIR]</td></tr>");
                await!(tx.send(Bytes::from(w)));

                for dirent in &dirents {
                    let modified = match dirent.meta.modified() {
                        Ok(t) => {
                            let tm = time::at(systemtime_to_timespec(t));
                            format!(
                                "{:04}-{:02}-{:02} {:02}:{:02}",
                                tm.tm_year + 1900,
                                tm.tm_mon + 1,
                                tm.tm_mday,
                                tm.tm_hour,
                                tm.tm_min
                            )
                        },
                        Err(_) => "".to_string(),
                    };
                    let size = match dirent.meta.is_file() {
                        true => dirent.meta.len().to_string(),
                        false => "[DIR]".to_string(),
                    };
                    let name = htmlescape::encode_minimal(&dirent.name);
                    let s = format!("<tr><td><a href=\"{}\">{}</a></td><td class=\"mono\">{}</td><td class=\"mono\" align=\"right\">{}</td></tr>",
                             dirent.path, name, modified, size);
                    await!(tx.send(Bytes::from(s)));
                }

                let mut w = String::new();
                w.push_str("<tr><th colspan=\"3\"><hr></th></tr>");
                w.push_str("</table></body></html>");
                await!(tx.send(Bytes::from(w)));

                Ok::<_, std::io::Error>(())
            }));

            Ok(res)
        }
    }
}
