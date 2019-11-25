use std::cmp;
use std::io::Write;

use futures::StreamExt;
use headers::HeaderMapExt;
use htmlescape;
use http::{status::StatusCode, Request, Response};
use time;

use bytes::Bytes;

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::conditional;
use crate::davheaders;
use crate::errors::*;
use crate::fs::*;
use crate::util::systemtime_to_timespec;
use crate::Method;

struct Range {
    start: u64,
    count: u64,
}

const BOUNDARY: &str = "BOUNDARY";
const BOUNDARY_START: &str = "\n--BOUNDARY\n";
const BOUNDARY_END: &str = "\n--BOUNDARY--\n";

const READ_BUF_SIZE: usize = 16384;

impl crate::DavInner {
    pub(crate) async fn handle_get(&self, req: Request<()>) -> DavResult<Response<Body>> {
        //let filesystem = self.fs.as_ref().ok_or(DavError::Status(StatusCode::METHOD_NOT_ALLOWED))?;
        let filesystem = &self.fs;
        let path = self.path(&req);

        // check if it's a directory.
        let head = req.method() == &http::Method::HEAD;
        let meta = filesystem.metadata(&path).await?;
        if meta.is_dir() {
            return self.handle_dirlist(req, head, filesystem).await;
        }

        // double check, is it a regular file.
        let mut file = filesystem.open(&path, OpenOptions::read()).await?;
        let meta = file.metadata().await?;
        if !meta.is_file() {
            return Err(DavError::Status(StatusCode::METHOD_NOT_ALLOWED));
        }
        let len = meta.len();
        let mut curpos = 0u64;
        let file_etag = davheaders::ETag::from_meta(&meta);

        let mut ranges = Vec::new();
        let mut do_range = match req.headers().typed_try_get::<davheaders::IfRange>() {
            Ok(Some(r)) => conditional::ifrange_match(&r, file_etag.as_ref(), meta.modified().ok()),
            Ok(None) => true,
            Err(_) => false,
        };

        let mut res = Response::new(Body::empty());
        let mut no_body = false;

        // set Last-Modified and ETag headers.
        if let Ok(modified) = meta.modified() {
            res.headers_mut()
                .typed_insert(headers::LastModified::from(modified));
        }
        if let Some(etag) = file_etag {
            res.headers_mut().typed_insert(etag);
        }

        // Apache always adds an Accept-Ranges header, even with partial
        // responses where it should be pretty obvious. So something somewhere
        // probably depends on that.
        res.headers_mut().typed_insert(headers::AcceptRanges::bytes());

        // handle the if-headers.
        if let Some(s) = conditional::if_match(&req, Some(&meta), filesystem, &self.ls, &path).await {
            *res.status_mut() = s;
            no_body = true;
            do_range = false;
        }

        // see if we want to get one or more ranges.
        if do_range {
            if let Some(r) = req.headers().typed_get::<headers::Range>() {
                debug!("handle_gethead: range header {:?}", r);
                use std::ops::Bound::*;
                for range in r.iter() {
                    let (start, mut count, valid) = match range {
                        (Included(s), Included(e)) if e >= s => (s, e - s + 1, true),
                        (Included(s), Unbounded) if s <= len => (s, len - s, true),
                        (Unbounded, Included(n)) if n <= len => (len - n, n, true),
                        _ => (0, 0, false),
                    };
                    if !valid || start >= len {
                        let r = format!("bytes */{}", len);
                        res.headers_mut().insert("Content-Range", r.parse().unwrap());
                        *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                        ranges.clear();
                        no_body = true;
                        break;
                    }
                    if start + count > len {
                        count = len - start;
                    }
                    ranges.push(Range { start, count });
                }
            }
        }

        if ranges.len() > 0 {
            // seek to beginning of the first range.
            if let Err(_) = file.seek(std::io::SeekFrom::Start(ranges[0].start)).await {
                let r = format!("bytes */{}", len);
                res.headers_mut().insert("Content-Range", r.parse().unwrap());
                *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                ranges.clear();
                no_body = true;
            }
        }

        if ranges.len() > 0 {
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
            ranges.push(Range { start: 0, count: len });
        }

        // set content-length and start if we're not doing multipart.
        let content_type = path.get_mime_type_str();
        if ranges.len() <= 1 {
            res.headers_mut()
                .typed_insert(davheaders::ContentType(content_type.to_owned()));
            let notmod = res.status() == StatusCode::NOT_MODIFIED;
            let len = if head || !no_body || notmod {
                ranges[0].count
            } else {
                0
            };
            res.headers_mut().typed_insert(headers::ContentLength(len));
        }

        if head || no_body {
            return Ok(res);
        }

        // now just loop and send data.
        *res.body_mut() = Body::from(AsyncStream::new(|mut tx| {
            async move {
                let mut buffer = [0; READ_BUF_SIZE];
                let zero = [0; 4096];

                let multipart = ranges.len() > 1;
                for range in ranges {
                    debug!("handle_get: start = {}, count = {}", range.start, range.count);
                    if curpos != range.start {
                        // this should never fail, but if it does, just skip this range
                        // and try the next one.
                        if let Err(_e) = file.seek(std::io::SeekFrom::Start(range.start)).await {
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
                        tx.send(Bytes::from(hdrs)).await;
                    }

                    let mut count = range.count;
                    while count > 0 {
                        let data;
                        let blen = cmp::min(count, READ_BUF_SIZE as u64) as usize;
                        let mut n = file.read_bytes(&mut buffer[..blen]).await?;
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
                        tx.send(Bytes::from(data)).await;
                    }
                }
                if multipart {
                    tx.send(Bytes::from(BOUNDARY_END)).await;
                }
                Ok::<(), std::io::Error>(())
            }
        }));

        Ok(res)
    }

    pub(crate) async fn handle_dirlist(&self, req: Request<()>, head: bool, filesystem: &Box<dyn DavFileSystem>) -> DavResult<Response<Body>> {
        let mut res = Response::new(Body::empty());

        // This is a directory. If the path doesn't end in "/", send a redir.
        // Most webdav clients handle redirect really bad, but a client asking
        // for a directory index is usually a browser.
        let path = self.path(&req);
        if !path.is_collection() {
            let mut path = path.clone();
            path.add_slash();
            res.headers_mut()
                .insert("Location", path.as_utf8_string_with_prefix().parse().unwrap());
            res.headers_mut().typed_insert(headers::ContentLength(0));
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
        let mut entries = filesystem.read_dir(&path, ReadDirMeta::Data).await?;

        // start output
        res.headers_mut()
            .insert("Content-Type", "text/html; charset=utf-8".parse().unwrap());
        *res.status_mut() = StatusCode::OK;
        if head {
            return Ok(res);
        }

        // now just loop and send data.
        *res.body_mut() = Body::from(AsyncStream::new(|mut tx| {
            async move {
                // transform all entries into a dirent struct.
                struct Dirent {
                    path: String,
                    name: String,
                    meta: Box<dyn DavMetaData>,
                }

                let mut dirents: Vec<Dirent> = Vec::new();
                while let Some(dirent) = entries.next().await {
                    let mut name = dirent.name();
                    if name.starts_with(b".") {
                        continue;
                    }
                    let mut npath = path.clone();
                    npath.push_segment(&name);
                    if let Ok(meta) = dirent.metadata().await {
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
                tx.send(Bytes::from(w)).await;

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
                    tx.send(Bytes::from(s)).await;
                }

                let mut w = String::new();
                w.push_str("<tr><th colspan=\"3\"><hr></th></tr>");
                w.push_str("</table></body></html>");
                tx.send(Bytes::from(w)).await;

                Ok::<_, std::io::Error>(())
            }
        }));

        Ok(res)
    }
}
