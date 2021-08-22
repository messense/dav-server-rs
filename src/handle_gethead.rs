use std::cmp;
use std::io::Write;

use futures_util::StreamExt;
use headers::HeaderMapExt;
use htmlescape;
use http::{status::StatusCode, Request, Response};

use bytes::Bytes;

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::conditional;
use crate::davheaders;
use crate::davpath::DavPath;
use crate::errors::*;
use crate::fs::*;
use crate::util::systemtime_to_offsetdatetime;
use crate::DavMethod;

struct Range {
    start: u64,
    count: u64,
}

const BOUNDARY: &str = "BOUNDARY";
const BOUNDARY_START: &str = "\n--BOUNDARY\n";
const BOUNDARY_END: &str = "\n--BOUNDARY--\n";

const READ_BUF_SIZE: usize = 16384;

impl crate::DavInner {
    pub(crate) async fn handle_get(&self, req: &Request<()>) -> DavResult<Response<Body>> {
        let head = req.method() == &http::Method::HEAD;
        let mut path = self.path(&req);
        let mut is_hbs = false;

        // check if it's a directory.
        let meta = self.fs.metadata(&path).await?;
        if meta.is_dir() {
            //
            // This is a directory. If the path doesn't end in "/", send a redir.
            // Most webdav clients handle redirect really bad, but a client asking
            // for a directory index is usually a browser.
            //
            if !path.is_collection() {
                let mut res = Response::new(Body::empty());
                path.add_slash();
                res.headers_mut()
                    .insert("Location", path.with_prefix().as_url_string().parse().unwrap());
                res.headers_mut().typed_insert(headers::ContentLength(0));
                *res.status_mut() = StatusCode::FOUND;
                return Ok(res);
            }

            // If indexfile was set, use it.
            if let Some(indexfile) = self.indexfile.as_ref() {
                path.push_segment(indexfile.as_bytes());
                is_hbs = indexfile.ends_with(".hbs");
            } else {
                // Otherwise see if we need to generate a directory index.
                return self.handle_autoindex(req, head).await;
            }
        }

        // double check, is it a regular file.
        let mut file = self.fs.open(&path, OpenOptions::read()).await?;
        #[allow(unused_mut)]
        let mut meta = file.metadata().await?;
        if !meta.is_file() {
            return Err(DavError::Status(StatusCode::METHOD_NOT_ALLOWED));
        }

        // if it was a .hbs file, process it.
        #[cfg(feature = "handlebars")]
        if is_hbs {
            let (f, m) = read_handlebars(req, file).await?;
            file = f;
            meta = m;
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
        if let Some(s) = conditional::if_match(&req, Some(&meta), &self.fs, &self.ls, &path).await {
            *res.status_mut() = s;
            no_body = true;
            do_range = false;
        }

        // see if we want to get one or more ranges.
        if do_range {
            if let Some(r) = req.headers().typed_get::<headers::Range>() {
                trace!("handle_gethead: range header {:?}", r);
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
        let content_type = if is_hbs {
            "text/html; charset=UTF-8"
        } else {
            path.get_mime_type_str()
        };
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
        let read_buf_size = self.read_buf_size.unwrap_or(READ_BUF_SIZE);
        *res.body_mut() = Body::from(AsyncStream::new(|mut tx| {
            async move {
                let zero = [0; 4096];

                let multipart = ranges.len() > 1;
                for range in ranges {
                    trace!("handle_get: start = {}, count = {}", range.start, range.count);
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
                        let blen = cmp::min(count, read_buf_size as u64) as usize;
                        let mut buf = file.read_bytes(blen).await?;
                        if buf.len() == 0 {
                            // this is a cop out. if the file got truncated, just
                            // return zeroed bytes instead of file content.
                            let n = if count > 4096 { 4096 } else { count as usize };
                            buf = Bytes::copy_from_slice(&zero[..n]);
                        }
                        let len = buf.len() as u64;
                        count -= len;
                        curpos += len;
                        trace!("sending {} bytes", len);
                        tx.send(buf).await;
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

    pub(crate) async fn handle_autoindex(&self, req: &Request<()>, head: bool) -> DavResult<Response<Body>> {
        let mut res = Response::new(Body::empty());
        let path = self.path(&req);

        // Is PROPFIND explicitly allowed?
        let allow_propfind = self
            .allow
            .map(|x| x.contains(DavMethod::PropFind))
            .unwrap_or(false);

        // Only allow index generation if explicitly set to true, _or_ if it was
        // unset, and PROPFIND is explicitly allowed.
        if !self.autoindex.unwrap_or(allow_propfind) {
            debug!("method {} not allowed on request {}", req.method(), req.uri());
            return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
        }

        // read directory or bail.
        let mut entries = self.fs.read_dir(&path, ReadDirMeta::Data).await?;

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
                            path: npath.with_prefix().as_url_string(),
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
                let upath = htmlescape::encode_minimal(&path.with_prefix().as_url_string());
                let mut w = String::new();
                w.push_str(
                    "\
                    <html><head>\n\
                    <title>Index of ",
                );
                w.push_str(&upath);
                w.push_str("</title>\n");
                w.push_str(
                    "\
                    <style>\n\
                    table {\n\
                      border-collapse: separate;\n\
                      border-spacing: 1.5em 0.25em;\n\
                    }\n\
                    h1 {\n\
                      padding-left: 0.3em;\n\
                    }\n\
                    a {\n\
                      text-decoration: none;\n\
                      color: blue;\n\
                    }\n\
                    .left {\n\
                      text-align: left;\n\
                    }\n\
                    .mono {\n\
                      font-family: monospace;\n\
                    }\n\
                    .mw20 {\n\
                      min-width: 20em;\n\
                    }\n\
                    </style>\n\
                    </head>\n\
                    <body>\n",
                );
                w.push_str(&format!("<h1>Index of {}</h1>", display_path(&path)));
                w.push_str(
                    "\
                    <table>\n\
                    <tr>\n\
                      <th class=\"left mw20\">Name</th>\n\
                      <th class=\"left\">Last modified</th>\n\
                      <th>Size</th>\n\
                    </tr>\n\
                    <tr><th colspan=\"3\"><hr></th></tr>\n\
                    <tr>\n\
                      <td><a href=\"..\">Parent Directory</a></td>\n\
                      <td>&nbsp;</td>\n\
                      <td class=\"mono\" align=\"right\">[DIR]    </td>\n\
                    </tr>\n",
                );

                tx.send(Bytes::from(w)).await;

                for dirent in &dirents {
                    let modified = match dirent.meta.modified() {
                        Ok(t) => {
                            let tm = systemtime_to_offsetdatetime(t);
                            format!(
                                "{:04}-{:02}-{:02} {:02}:{:02}",
                                tm.year(),
                                tm.month(),
                                tm.day(),
                                tm.hour(),
                                tm.minute(),
                            )
                        },
                        Err(_) => "".to_string(),
                    };
                    let size = match dirent.meta.is_file() {
                        true => display_size(dirent.meta.len()),
                        false => "[DIR]    ".to_string(),
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

fn display_size(size: u64) -> String {
    if size <= 1000 {
        return format!("{}    ", size);
    }
    if size <= 1_000_000 {
        return format!("{} KiB", ((size / 10) as f64) / 100f64);
    }
    if size <= 1_000_000_000 {
        return format!("{} MiB", ((size / 10_000) as f64) / 100f64);
    }
    if size <= 1_000_000_000_000 {
        return format!("{} GiB", ((size / 10_000_000) as f64) / 100f64);
    }
    format!("{:2}TiB", ((size / 10_000_000_000) as f64) / 100f64)
}

fn display_path(path: &DavPath) -> String {
    let path_dsp = String::from_utf8_lossy(path.with_prefix().as_bytes());
    let path_url = path.with_prefix().as_url_string();
    let dpath_segs = path_dsp.split("/").filter(|s| !s.is_empty()).collect::<Vec<_>>();
    let upath_segs = path_url.split("/").filter(|s| !s.is_empty()).collect::<Vec<_>>();
    let mut dpath = String::new();
    let mut upath = String::new();

    if dpath_segs.len() == 0 {
        dpath.push_str("/");
    } else {
        dpath.push_str("<a href = \"/\">/</a>");
    }

    for idx in 0..dpath_segs.len() {
        upath.push('/');
        upath.push_str(upath_segs[idx]);
        let dseg = htmlescape::encode_minimal(dpath_segs[idx]);
        if idx == dpath_segs.len() - 1 {
            dpath.push_str(&dseg);
        } else {
            dpath.push_str(&format!("<a href = \"{}\">{}</a>/", upath, dseg));
        }
    }

    dpath
}

use std::time::SystemTime;
use crate::fs::{DavMetaData, FsResult};

#[cfg(feature = "handlebars")]
use std::collections::HashMap;
#[cfg(feature = "handlebars")]
use std::convert::TryInto;
#[cfg(feature = "handlebars")]
use std::io::{Error, ErrorKind, SeekFrom};
#[cfg(feature = "handlebars")]
use futures_util::future::{self, FutureExt};
#[cfg(feature = "handlebars")]
use headers::{authorization::Basic, Authorization};
#[cfg(feature = "handlebars")]
use handlebars::Handlebars;
#[cfg(feature = "handlebars")]
use crate::fs::{DavFile, FsFuture};

#[cfg(feature = "handlebars")]
async fn read_handlebars(
    req: &Request<()>,
    mut file: Box<dyn DavFile>,
) -> DavResult<(Box<dyn DavFile>, Box<dyn DavMetaData>)>
{
    let hbs = Handlebars::new();
    let mut vars = HashMap::new();
    let headers = req.headers();

    // Read .hbs file into memory.
    let len = file.metadata().await?.len();
    let buffer = file.read_bytes(len.try_into().unwrap()).await?;
    let data = std::str::from_utf8(&buffer)?;

    // Set variables.
    for hdr in &["User-Agent", "Host", "Referer"] {
        if let Some(val) = headers.get(*hdr) {
            let mut var = "HTTP_".to_string() + &hdr.replace('-', "_");
            var.make_ascii_uppercase();
            if let Ok(valstr) = val.to_str() {
                vars.insert(var, valstr.to_string());
            }
        }
    }
    match headers.typed_get::<Authorization<Basic>>() {
        Some(Authorization(basic)) => {
            vars.insert("AUTH_TYPE".to_string(), "Basic".to_string());
            vars.insert("REMOTE_USER".to_string(), basic.username().to_string());
        },
        _ => {},
    }

    // Render.
    let result = hbs
        .render_template(data, &vars)
        .map_err(|_| DavError::Status(StatusCode::INTERNAL_SERVER_ERROR))?;

    let mut hbsfile = HbsFile::new(result);
    let hbsmeta = hbsfile.metadata().await?;
    Ok((hbsfile, hbsmeta))
}

#[derive(Clone, Debug)]
struct HbsMeta {
    mtime: SystemTime,
    size:  u64,
}

impl DavMetaData for HbsMeta {
    fn len(&self) -> u64 {
        self.size
    }

    fn created(&self) -> FsResult<SystemTime> {
        Ok(self.mtime)
    }

    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.mtime)
    }

    fn is_dir(&self) -> bool {
        false
    }
}

#[cfg(feature = "handlebars")]
#[derive(Clone, Debug)]
struct HbsFile {
    meta: HbsMeta,
    pos:  usize,
    data: Vec<u8>,
}

#[cfg(feature = "handlebars")]
impl HbsFile {
    fn new(data: String) -> Box<dyn DavFile> {
        Box::new(HbsFile {
            meta: HbsMeta {
                mtime: SystemTime::now(),
                size:  data.len() as u64,
            },
            data: data.into_bytes(),
            pos:  0,
        })
    }
}

#[cfg(feature = "handlebars")]
impl DavFile for HbsFile {
    fn metadata<'a>(&'a mut self) -> FsFuture<Box<dyn DavMetaData>> {
        async move { Ok(Box::new(self.meta.clone()) as Box<dyn DavMetaData>) }.boxed()
    }

    fn read_bytes<'a>(&'a mut self, count: usize) -> FsFuture<Bytes> {
        async move {
            let start = self.pos;
            let end = std::cmp::min(self.pos + count, self.data.len());
            self.pos += end - start;
            let b = Bytes::copy_from_slice(&self.data[start..end]);
            Ok(b)
        }
        .boxed()
    }

    fn seek<'a>(&'a mut self, pos: SeekFrom) -> FsFuture<u64> {
        async move {
            let (start, offset): (u64, i64) = match pos {
                SeekFrom::Start(npos) => (0, npos as i64),
                SeekFrom::Current(npos) => (self.pos as u64, npos),
                SeekFrom::End(npos) => (self.data.len() as u64, npos),
            };
            if offset < 0 {
                if -offset as u64 > start {
                    return Err(Error::new(ErrorKind::InvalidInput, "invalid seek").into());
                }
                self.pos = (start - (-offset as u64)) as usize;
            } else {
                self.pos = (start + offset as u64) as usize;
            }
            Ok(self.pos as u64)
        }
        .boxed()
    }

    fn write_buf<'a>(&'a mut self, _buf: Box<dyn bytes::Buf + Send>) -> FsFuture<()> {
        Box::pin(future::ready(Err(FsError::NotImplemented)))
    }

    fn write_bytes<'a>(&'a mut self, _buf: bytes::Bytes) -> FsFuture<()> {
        Box::pin(future::ready(Err(FsError::NotImplemented)))
    }

    fn flush<'a>(&'a mut self) -> FsFuture<()> {
        Box::pin(future::ready(Ok(())))
    }
}
