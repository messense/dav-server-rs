use futures::{Future,StreamExt};
use htmlescape;
use http::{status::StatusCode, Request, Response};
use time;

use bytes::Bytes;

use crate::BoxedByteStream;
use crate::conditional;
use crate::corostream::CoroStream;
use crate::davheaders;
use crate::errors::*;
use crate::fs::*;
use crate::typed_headers::{self, ByteRangeSpec, HeaderMapExt};
use crate::util::{empty_body,systemtime_to_httpdate,systemtime_to_timespec};

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

            let mut start = 0;
            let mut count = meta.len();
            let len = count;
            let mut do_range = true;

            let file_etag = typed_headers::EntityTag::new(false, meta.etag());

            if let Some(r) = req.headers().typed_get::<davheaders::IfRange>() {
                do_range = conditional::ifrange_match(&r, &file_etag, meta.modified().unwrap());
            }

            // see if we want to get a range.
            if do_range {
                do_range = false;
                if let Some(r) = req.headers().typed_get::<typed_headers::Range>() {
                    match r {
                        typed_headers::Range::Bytes(ref ranges) => {
                            // we only support a single range
                            if ranges.len() == 1 {
                                match &ranges[0] {
                                    &ByteRangeSpec::FromTo(s, e) => {
                                        start = s;
                                        count = e - s + 1;
                                    },
                                    &ByteRangeSpec::AllFrom(s) => {
                                        start = s;
                                        count = len - s;
                                    },
                                    &ByteRangeSpec::Last(n) => {
                                        start = len - n;
                                        count = n;
                                    },
                                }
                                if start >= len {
                                    return Err(DavError::Status(StatusCode::RANGE_NOT_SATISFIABLE));
                                }
                                if start + count > len {
                                    count = len - start;
                                }
                                do_range = true;
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
                    .typed_insert(typed_headers::LastModified(systemtime_to_httpdate(modified)));
            }
            res.headers_mut().typed_insert(typed_headers::ETag(file_etag));

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

            if do_range {
                // seek to beginning of requested data.
                if let Err(_) = await!(file.seek(std::io::SeekFrom::Start(start))) {
                    *res.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                    return Ok(res);
                }

                // set partial-content status and add content-range header.
                let r = format!("bytes {}-{}/{}", start, start + count - 1, len);
                res.headers_mut().insert("Content-Range", r.parse().unwrap());
                *res.status_mut() = StatusCode::PARTIAL_CONTENT;
            } else {
                // normal request, send entire file.
                *res.status_mut() = StatusCode::OK;
            }

            // set content-length and start.
            res.headers_mut()
                .insert("Content-Type", path.get_mime_type_str().parse().unwrap());
            res.headers_mut()
                .typed_insert(typed_headers::ContentLength(count));
            res.headers_mut()
                .typed_insert(typed_headers::AcceptRanges(vec![typed_headers::RangeUnit::Bytes]));

            debug!("head is {}", head);
            if head {
                return Ok(res);
            }

            // now just loop and send data.
            *res.body_mut() = Box::new(CoroStream::stream01(async move |mut tx| {
                let mut buffer = [0; 8192];
                let zero = [0; 4096];

                debug!("count = {}", count);

                while count > 0 {
                    let data;
                    let mut n = await!(file.read_bytes(&mut buffer[..]))?;
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
                    debug!("sending {} bytes", data.len());
                    await!(tx.send(Bytes::from(data)));
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
            *res.body_mut() = Box::new(CoroStream::stream01(async move |mut tx| {
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
                let upath = htmlescape::encode_minimal(&path.as_url_string());
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
