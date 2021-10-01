use std::borrow::Cow;
use std::collections::HashMap;
use std::convert::TryFrom;
use std::io::{self, Cursor};

use bytes::Bytes;
use futures_util::{future::BoxFuture, FutureExt, StreamExt};
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::xmltree_ext::*;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;
use xml::EmitterConfig;
use xmltree::{Element, XMLNode};

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::conditional::if_match_get_tokens;
use crate::davheaders;
use crate::davpath::*;
use crate::errors::*;
use crate::fs::*;
use crate::handle_lock::{list_lockdiscovery, list_supportedlock};
use crate::ls::*;
use crate::util::MemBuffer;
use crate::util::{dav_xml_error, systemtime_to_httpdate, systemtime_to_rfc3339};
use crate::{DavInner, DavResult};

const NS_APACHE_URI: &'static str = "http://apache.org/dav/props/";
const NS_DAV_URI: &'static str = "DAV:";
const NS_MS_URI: &'static str = "urn:schemas-microsoft-com:";

// list returned by PROPFIND <propname/>.
const PROPNAME_STR: &'static [&'static str] = &[
    "D:creationdate",
    "D:displayname",
    "D:getcontentlanguage",
    "D:getcontentlength",
    "D:getcontenttype",
    "D:getetag",
    "D:getlastmodified",
    "D:lockdiscovery",
    "D:resourcetype",
    "D:supportedlock",
    "D:quota-available-bytes",
    "D:quota-used-bytes",
    "A:executable",
    "Z:Win32LastAccessTime",
];

// properties returned by PROPFIND <allprop/> or empty body.
const ALLPROP_STR: &'static [&'static str] = &[
    "D:creationdate",
    "D:displayname",
    "D:getcontentlanguage",
    "D:getcontentlength",
    "D:getcontenttype",
    "D:getetag",
    "D:getlastmodified",
    "D:lockdiscovery",
    "D:resourcetype",
    "D:supportedlock",
];

// properties returned by PROPFIND with empty body for Microsoft clients.
const MS_ALLPROP_STR: &'static [&'static str] = &[
    "D:creationdate",
    "D:displayname",
    "D:getcontentlanguage",
    "D:getcontentlength",
    "D:getcontenttype",
    "D:getetag",
    "D:getlastmodified",
    "D:lockdiscovery",
    "D:resourcetype",
    "D:supportedlock",
    "Z:Win32CreationTime",
    "Z:Win32FileAttributes",
    "Z:Win32LastAccessTime",
    "Z:Win32LastModifiedTime",
];

lazy_static! {
    static ref ALLPROP: Vec<Element> = init_staticprop(ALLPROP_STR);
    static ref MS_ALLPROP: Vec<Element> = init_staticprop(MS_ALLPROP_STR);
    static ref PROPNAME: Vec<Element> = init_staticprop(PROPNAME_STR);
}

type Emitter = EventWriter<MemBuffer>;
type Sender = crate::async_stream::Sender<bytes::Bytes, io::Error>;

struct StatusElement {
    status:  StatusCode,
    element: Element,
}

struct PropWriter {
    emitter:   Emitter,
    tx:        Option<Sender>,
    name:      String,
    props:     Vec<Element>,
    fs:        Box<dyn DavFileSystem>,
    ls:        Option<Box<dyn DavLockSystem>>,
    useragent: String,
    q_cache:   QuotaCache,
}

#[derive(Default, Clone, Copy)]
struct QuotaCache {
    q_state: u32,
    q_used:  u64,
    q_total: Option<u64>,
}

fn init_staticprop(p: &[&str]) -> Vec<Element> {
    let mut v = Vec::new();
    for a in p {
        let mut e = Element::new2(*a);
        e.namespace = match e.prefix.as_ref().map(|x| x.as_str()) {
            Some("D") => Some(NS_DAV_URI.to_string()),
            Some("A") => Some(NS_APACHE_URI.to_string()),
            Some("Z") => Some(NS_MS_URI.to_string()),
            _ => None,
        };
        v.push(e);
    }
    v
}

impl DavInner {
    pub(crate) async fn handle_propfind(
        self,
        req: &Request<()>,
        xmldata: &[u8],
    ) -> DavResult<Response<Body>>
    {
        // No checks on If: and If-* headers here, because I do not see
        // the point and there's nothing in RFC4918 that indicates we should.

        let mut res = Response::new(Body::empty());

        res.headers_mut()
            .typed_insert(headers::CacheControl::new().with_no_cache());
        res.headers_mut().typed_insert(headers::Pragma::no_cache());

        let depth = match req.headers().typed_get::<davheaders::Depth>() {
            Some(davheaders::Depth::Infinity) | None => {
                if req.headers().typed_get::<davheaders::XLitmus>().is_none() {
                    let ct = "application/xml; charset=utf-8".to_owned();
                    res.headers_mut().typed_insert(davheaders::ContentType(ct));
                    *res.status_mut() = StatusCode::FORBIDDEN;
                    *res.body_mut() = dav_xml_error("<D:propfind-finite-depth/>");
                    return Ok(res);
                }
                davheaders::Depth::Infinity
            },
            Some(d) => d.clone(),
        };

        // path and meta
        let mut path = self.path(&req);
        let meta = self.fs.metadata(&path).await?;
        let meta = self.fixpath(&mut res, &mut path, meta);

        let mut root = None;
        if xmldata.len() > 0 {
            root = match Element::parse(Cursor::new(xmldata)) {
                Ok(t) => {
                    if t.name == "propfind" && t.namespace.as_ref().map(|s| s.as_str()) == Some("DAV:") {
                        Some(t)
                    } else {
                        return Err(DavError::XmlParseError.into());
                    }
                },
                Err(_) => return Err(DavError::XmlParseError.into()),
            };
        }

        let (name, props) = match root {
            None => ("allprop", Vec::new()),
            Some(mut elem) => {
                let includes = elem
                    .take_child("includes")
                    .map_or(Vec::new(), |n| n.take_child_elems());
                match elem
                    .child_elems_into_iter()
                    .find(|e| e.name == "propname" || e.name == "prop" || e.name == "allprop")
                {
                    Some(elem) => {
                        match elem.name.as_str() {
                            "propname" => ("propname", Vec::new()),
                            "prop" => ("prop", elem.take_child_elems()),
                            "allprop" => ("allprop", includes),
                            _ => return Err(DavError::XmlParseError.into()),
                        }
                    },
                    None => return Err(DavError::XmlParseError.into()),
                }
            },
        };

        trace!("propfind: type request: {}", name);

        let mut pw = PropWriter::new(&req, &mut res, name, props, &self.fs, self.ls.as_ref())?;

        *res.body_mut() = Body::from(AsyncStream::new(|tx| {
            async move {
                pw.set_tx(tx);
                let is_dir = meta.is_dir();
                pw.write_props(&path, meta).await?;
                pw.flush().await?;

                if is_dir && depth != davheaders::Depth::Zero {
                    let _ = self.propfind_directory(&path, depth, &mut pw).await;
                }
                pw.close().await?;

                Ok(())
            }
        }));

        Ok(res)
    }

    fn propfind_directory<'a>(
        &'a self,
        path: &'a DavPath,
        depth: davheaders::Depth,
        propwriter: &'a mut PropWriter,
    ) -> BoxFuture<'a, DavResult<()>>
    {
        async move {
            let readdir_meta = match self.hide_symlinks {
                Some(true) | None => ReadDirMeta::DataSymlink,
                Some(false) => ReadDirMeta::Data,
            };
            let mut entries = match self.fs.read_dir(path, readdir_meta).await {
                Ok(entries) => entries,
                Err(e) => {
                    // if we cannot read_dir, just skip it.
                    error!("read_dir error {:?}", e);
                    return Ok(());
                },
            };

            while let Some(dirent) = entries.next().await {
                let mut npath = path.clone();
                npath.push_segment(&dirent.name());
                let meta = match dirent.metadata().await {
                    Ok(meta) => meta,
                    Err(e) => {
                        trace!("metadata error on {}. Skipping {:?}", npath, e);
                        continue;
                    },
                };
                if meta.is_symlink() {
                    continue;
                }
                if meta.is_dir() {
                    npath.add_slash();
                }
                let is_dir = meta.is_dir();
                propwriter.write_props(&npath, meta).await?;
                propwriter.flush().await?;
                if depth == davheaders::Depth::Infinity && is_dir {
                    self.propfind_directory(&npath, depth, propwriter).await?;
                }
            }
            Ok(())
        }
        .boxed()
    }

    // set/change a live property. returns StatusCode::CONTINUE if
    // this wasnt't  a live property (or, if we want it handled
    // as a dead property, e.g. DAV:displayname).
    fn liveprop_set(&self, prop: &Element, can_deadprop: bool) -> StatusCode {
        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => {
                match prop.name.as_str() {
                    "getcontentlanguage" => {
                        if prop.get_text().is_none() || prop.has_child_elems() {
                            return StatusCode::CONFLICT;
                        }
                        // FIXME only here to make "litmus" happy, really...
                        if let Some(s) = prop.get_text() {
                            if davheaders::ContentLanguage::try_from(s.as_ref()).is_err() {
                                return StatusCode::CONFLICT;
                            }
                        }
                        if can_deadprop {
                            StatusCode::CONTINUE
                        } else {
                            StatusCode::FORBIDDEN
                        }
                    },
                    "displayname" => {
                        if prop.get_text().is_none() || prop.has_child_elems() {
                            return StatusCode::CONFLICT;
                        }
                        if can_deadprop {
                            StatusCode::CONTINUE
                        } else {
                            StatusCode::FORBIDDEN
                        }
                    },
                    "getlastmodified" => {
                        // we might allow setting modified time
                        // by using utimes() on unix. Not yet though.
                        if prop.get_text().is_none() || prop.has_child_elems() {
                            return StatusCode::CONFLICT;
                        }
                        StatusCode::FORBIDDEN
                    },
                    _ => StatusCode::FORBIDDEN,
                }
            },
            Some(NS_APACHE_URI) => {
                match prop.name.as_str() {
                    "executable" => {
                        // we could allow toggling the execute bit.
                        // to be implemented.
                        if prop.get_text().is_none() || prop.has_child_elems() {
                            return StatusCode::CONFLICT;
                        }
                        StatusCode::FORBIDDEN
                    },
                    _ => StatusCode::FORBIDDEN,
                }
            },
            Some(NS_MS_URI) => {
                match prop.name.as_str() {
                    "Win32CreationTime" |
                    "Win32FileAttributes" |
                    "Win32LastAccessTime" |
                    "Win32LastModifiedTime" => {
                        if prop.get_text().is_none() || prop.has_child_elems() {
                            return StatusCode::CONFLICT;
                        }
                        // Always report back that we successfully
                        // changed these, even if we didn't --
                        // makes the windows webdav client work.
                        StatusCode::OK
                    },
                    _ => StatusCode::FORBIDDEN,
                }
            },
            _ => StatusCode::CONTINUE,
        }
    }

    // In general, live properties cannot be removed, with the
    // exception of getcontentlanguage and displayname.
    fn liveprop_remove(&self, prop: &Element, can_deadprop: bool) -> StatusCode {
        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => {
                match prop.name.as_str() {
                    "getcontentlanguage" | "displayname" => {
                        if can_deadprop {
                            StatusCode::OK
                        } else {
                            StatusCode::FORBIDDEN
                        }
                    },
                    _ => StatusCode::FORBIDDEN,
                }
            },
            Some(NS_APACHE_URI) | Some(NS_MS_URI) => StatusCode::FORBIDDEN,
            _ => StatusCode::CONTINUE,
        }
    }

    pub(crate) async fn handle_proppatch(
        self,
        req: &Request<()>,
        xmldata: &[u8],
    ) -> DavResult<Response<Body>>
    {
        let mut res = Response::new(Body::empty());

        // file must exist.
        let mut path = self.path(&req);
        let meta = self.fs.metadata(&path).await?;
        let meta = self.fixpath(&mut res, &mut path, meta);

        // check the If and If-* headers.
        let tokens = match if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path).await {
            Ok(t) => t,
            Err(s) => return Err(s.into()),
        };

        // if locked check if we hold that lock.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if let Err(_l) = locksystem.check(&path, principal, false, false, t) {
                return Err(StatusCode::LOCKED.into());
            }
        }

        trace!(target: "xml", "proppatch input:\n{}]\n",
               std::string::String::from_utf8_lossy(&xmldata));

        // parse xml
        let tree = Element::parse2(Cursor::new(xmldata))?;
        if tree.name != "propertyupdate" {
            return Err(DavError::XmlParseError);
        }

        let mut patch = Vec::new();
        let mut ret = Vec::new();
        let can_deadprop = self.fs.have_props(&path).await;

        // walk over the element tree and feed "set" and "remove" items to
        // the liveprop_set/liveprop_remove functions. If skipped by those,
        // gather .them in the "patch" Vec to be processed as dead properties.
        for elem in tree.child_elems_iter() {
            for n in elem
                .child_elems_iter()
                .filter(|e| e.name == "prop")
                .flat_map(|e| e.child_elems_iter())
            {
                match elem.name.as_str() {
                    "set" => {
                        match self.liveprop_set(&n, can_deadprop) {
                            StatusCode::CONTINUE => patch.push((true, element_to_davprop_full(&n))),
                            s => ret.push((s, element_to_davprop(&n))),
                        }
                    },
                    "remove" => {
                        match self.liveprop_remove(&n, can_deadprop) {
                            StatusCode::CONTINUE => patch.push((false, element_to_davprop(&n))),
                            s => ret.push((s, element_to_davprop(&n))),
                        }
                    },
                    _ => {},
                }
            }
        }

        // if any set/remove failed, stop processing here.
        if ret.iter().any(|&(ref s, _)| s != &StatusCode::OK) {
            ret = ret
                .into_iter()
                .map(|(s, p)| {
                    if s == StatusCode::OK {
                        (StatusCode::FAILED_DEPENDENCY, p)
                    } else {
                        (s, p)
                    }
                })
                .collect::<Vec<_>>();
            ret.extend(patch.into_iter().map(|(_, p)| (StatusCode::FAILED_DEPENDENCY, p)));
        } else if patch.len() > 0 {
            // hmmm ... we assume nothing goes wrong here at the
            // moment. if it does, we should roll back the earlier
            // made changes to live props, but come on, we're not
            // builing a transaction engine here.
            let deadret = self.fs.patch_props(&path, patch).await?;
            ret.extend(deadret.into_iter());
        }

        // group by statuscode.
        let mut hm = HashMap::new();
        for (code, prop) in ret.into_iter() {
            if !hm.contains_key(&code) {
                hm.insert(code, Vec::new());
            }
            let v = hm.get_mut(&code).unwrap();
            v.push(davprop_to_element(prop));
        }

        // And reply.
        let mut pw = PropWriter::new(&req, &mut res, "propertyupdate", Vec::new(), &self.fs, None)?;
        *res.body_mut() = Body::from(AsyncStream::new(|tx| {
            async move {
                pw.set_tx(tx);
                pw.write_propresponse(&path, hm)?;
                pw.close().await?;
                Ok::<_, io::Error>(())
            }
        }));

        Ok(res)
    }
}

impl PropWriter {
    pub fn new(
        req: &Request<()>,
        res: &mut Response<Body>,
        name: &str,
        mut props: Vec<Element>,
        fs: &Box<dyn DavFileSystem>,
        ls: Option<&Box<dyn DavLockSystem>>,
    ) -> DavResult<PropWriter>
    {
        let contenttype = "application/xml; charset=utf-8".parse().unwrap();
        res.headers_mut().insert("content-type", contenttype);
        *res.status_mut() = StatusCode::MULTI_STATUS;

        let mut emitter = EventWriter::new_with_config(
            MemBuffer::new(),
            EmitterConfig {
                normalize_empty_elements: false,
                perform_indent: false,
                indent_string: Cow::Borrowed(""),
                ..Default::default()
            },
        );
        emitter.write(XmlWEvent::StartDocument {
            version:    XmlVersion::Version10,
            encoding:   Some("utf-8"),
            standalone: None,
        })?;

        // user-agent header.
        let ua = match req.headers().get("user-agent") {
            Some(s) => s.to_str().unwrap_or(""),
            None => "",
        };

        if name != "prop" && name != "propertyupdate" {
            let mut v = Vec::new();
            let iter = if name == "allprop" {
                if ua.contains("Microsoft") {
                    MS_ALLPROP.iter()
                } else {
                    ALLPROP.iter()
                }
            } else {
                PROPNAME.iter()
            };
            for a in iter {
                if !props
                    .iter()
                    .any(|e| a.namespace == e.namespace && a.name == e.name)
                {
                    v.push(a.clone());
                }
            }
            props.append(&mut v);
        }

        // check the prop namespaces to see what namespaces
        // we need to put in the preamble.
        let mut ev = XmlWEvent::start_element("D:multistatus").ns("D", NS_DAV_URI);
        if name != "propertyupdate" {
            let mut a = false;
            let mut m = false;
            for prop in &props {
                match prop.namespace.as_ref().map(|x| x.as_str()) {
                    Some(NS_APACHE_URI) => a = true,
                    Some(NS_MS_URI) => m = true,
                    _ => {},
                }
            }
            if a {
                ev = ev.ns("A", NS_APACHE_URI);
            }
            if m {
                ev = ev.ns("Z", NS_MS_URI);
            }
        }
        emitter.write(ev)?;

        Ok(PropWriter {
            emitter:   emitter,
            tx:        None,
            name:      name.to_string(),
            props:     props,
            fs:        fs.clone(),
            ls:        ls.map(|ls| ls.clone()),
            useragent: ua.to_string(),
            q_cache:   Default::default(),
        })
    }

    pub fn set_tx(&mut self, tx: Sender) {
        self.tx = Some(tx)
    }

    fn build_elem<T>(&self, content: bool, pfx: &str, e: &Element, text: T) -> DavResult<StatusElement>
    where T: Into<String> {
        let mut elem = Element {
            prefix:     Some(pfx.to_string()),
            namespace:  None,
            namespaces: None,
            name:       e.name.clone(),
            attributes: HashMap::new(),
            children:   Vec::new(),
        };
        if content {
            let t: String = text.into();
            if t != "" {
                elem.children.push(XMLNode::Text(t));
            }
        }
        Ok(StatusElement {
            status:  StatusCode::OK,
            element: elem,
        })
    }

    async fn get_quota<'a>(
        &'a self,
        qc: &'a mut QuotaCache,
        path: &'a DavPath,
        meta: &'a dyn DavMetaData,
    ) -> FsResult<(u64, Option<u64>)>
    {
        // do lookup only once.
        match qc.q_state {
            0 => {
                match self.fs.get_quota().await {
                    Err(e) => {
                        qc.q_state = 1;
                        return Err(e);
                    },
                    Ok((u, t)) => {
                        qc.q_used = u;
                        qc.q_total = t;
                        qc.q_state = 2;
                    },
                }
            },
            1 => return Err(FsError::NotImplemented),
            _ => {},
        }

        // if not "/", return for "used" just the size of this file/dir.
        let used = if path.as_bytes() == b"/" {
            qc.q_used
        } else {
            meta.len()
        };

        // calculate available space.
        let avail = match qc.q_total {
            None => None,
            Some(total) => Some(if total > used { total - used } else { 0 }),
        };
        Ok((used, avail))
    }

    async fn build_prop<'a>(
        &'a self,
        prop: &'a Element,
        path: &'a DavPath,
        meta: &'a dyn DavMetaData,
        qc: &'a mut QuotaCache,
        docontent: bool,
    ) -> DavResult<StatusElement>
    {
        // in some cases, a live property might be stored in the
        // dead prop database, like DAV:displayname.
        let mut try_deadprop = false;
        let mut pfx = "";

        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => {
                pfx = "D";
                match prop.name.as_str() {
                    "creationdate" => {
                        if let Ok(time) = meta.created() {
                            let tm = systemtime_to_rfc3339(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                        // use ctime instead - apache seems to do this.
                        if let Ok(ctime) = meta.status_changed() {
                            let mut time = ctime;
                            if let Ok(mtime) = meta.modified() {
                                if mtime < ctime {
                                    time = mtime;
                                }
                            }
                            let tm = systemtime_to_rfc3339(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                    },
                    "displayname" | "getcontentlanguage" => {
                        try_deadprop = true;
                    },
                    "getetag" => {
                        if let Some(etag) = meta.etag() {
                            return self.build_elem(docontent, pfx, prop, etag);
                        }
                    },
                    "getcontentlength" => {
                        if !meta.is_dir() {
                            return self.build_elem(docontent, pfx, prop, meta.len().to_string());
                        }
                    },
                    "getcontenttype" => {
                        return if meta.is_dir() {
                            self.build_elem(docontent, pfx, prop, "httpd/unix-directory")
                        } else {
                            self.build_elem(docontent, pfx, prop, path.get_mime_type_str())
                        };
                    },
                    "getlastmodified" => {
                        if let Ok(time) = meta.modified() {
                            let tm = systemtime_to_httpdate(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                    },
                    "resourcetype" => {
                        let mut elem = prop.clone();
                        if meta.is_dir() && docontent {
                            let dir = Element::new2("D:collection");
                            elem.children.push(XMLNode::Element(dir));
                        }
                        return Ok(StatusElement {
                            status:  StatusCode::OK,
                            element: elem,
                        });
                    },
                    "supportedlock" => {
                        return Ok(StatusElement {
                            status:  StatusCode::OK,
                            element: list_supportedlock(self.ls.as_ref()),
                        });
                    },
                    "lockdiscovery" => {
                        return Ok(StatusElement {
                            status:  StatusCode::OK,
                            element: list_lockdiscovery(self.ls.as_ref(), path),
                        });
                    },
                    "quota-available-bytes" => {
                        let mut qc = qc;
                        if let Ok((_, Some(avail))) = self.get_quota(&mut qc, path, meta).await {
                            return self.build_elem(docontent, pfx, prop, avail.to_string());
                        }
                    },
                    "quota-used-bytes" => {
                        let mut qc = qc;
                        if let Ok((used, _)) = self.get_quota(&mut qc, path, meta).await {
                            let used = if self.useragent.contains("WebDAVFS") {
                                // Need this on MacOs, otherwise the value is off
                                // by a factor of 10 or so .. ?!?!!?
                                format!("{:014}", used)
                            } else {
                                used.to_string()
                            };
                            return self.build_elem(docontent, pfx, prop, used);
                        }
                    },
                    _ => {},
                }
            },
            Some(NS_APACHE_URI) => {
                pfx = "A";
                match prop.name.as_str() {
                    "executable" => {
                        if let Ok(x) = meta.executable() {
                            let b = if x { "T" } else { "F" };
                            return self.build_elem(docontent, pfx, prop, b);
                        }
                    },
                    _ => {},
                }
            },
            Some(NS_MS_URI) => {
                pfx = "Z";
                match prop.name.as_str() {
                    "Win32CreationTime" => {
                        if let Ok(time) = meta.created() {
                            let tm = systemtime_to_httpdate(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                        // use ctime instead - apache seems to do this.
                        if let Ok(ctime) = meta.status_changed() {
                            let mut time = ctime;
                            if let Ok(mtime) = meta.modified() {
                                if mtime < ctime {
                                    time = mtime;
                                }
                            }
                            let tm = systemtime_to_httpdate(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                    },
                    "Win32LastAccessTime" => {
                        if let Ok(time) = meta.accessed() {
                            let tm = systemtime_to_httpdate(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                    },
                    "Win32LastModifiedTime" => {
                        if let Ok(time) = meta.modified() {
                            let tm = systemtime_to_httpdate(time);
                            return self.build_elem(docontent, pfx, prop, tm);
                        }
                    },
                    "Win32FileAttributes" => {
                        let mut attr = 0u32;
                        // Enable when we implement permissions() on DavMetaData.
                        //if meta.permissions().readonly() {
                        //    attr |= 0x0001;
                        //}
                        if path.file_name_bytes().starts_with(b".") {
                            attr |= 0x0002;
                        }
                        if meta.is_dir() {
                            attr |= 0x0010;
                        } else {
                            // this is the 'Archive' bit, which is set by
                            // default on _all_ files on creation and on
                            // modification.
                            attr |= 0x0020;
                        }
                        return self.build_elem(docontent, pfx, prop, format!("{:08x}", attr));
                    },
                    _ => {},
                }
            },
            _ => {
                try_deadprop = true;
            },
        }

        if try_deadprop && self.name == "prop" && self.fs.have_props(path).await {
            // asking for a specific property.
            let dprop = element_to_davprop(prop);
            if let Ok(xml) = self.fs.get_prop(path, dprop).await {
                if let Ok(e) = Element::parse(Cursor::new(xml)) {
                    return Ok(StatusElement {
                        status:  StatusCode::OK,
                        element: e,
                    });
                }
            }
        }
        let prop = if pfx != "" {
            self.build_elem(false, pfx, prop, "").map(|s| s.element).unwrap()
        } else {
            prop.clone()
        };
        Ok(StatusElement {
            status:  StatusCode::NOT_FOUND,
            element: prop,
        })
    }

    pub async fn write_props<'a>(
        &'a mut self,
        path: &'a DavPath,
        meta: Box<dyn DavMetaData + 'static>,
    ) -> Result<(), DavError>
    {
        // A HashMap<StatusCode, Vec<Element>> for the result.
        let mut props = HashMap::new();

        // Get properties one-by-one
        let do_content = self.name != "propname";
        let mut qc = self.q_cache;
        for p in &self.props {
            let res = self.build_prop(p, path, &*meta, &mut qc, do_content).await?;
            if res.status == StatusCode::OK || (self.name != "propname" && self.name != "allprop") {
                add_sc_elem(&mut props, res.status, res.element);
            }
        }
        self.q_cache = qc;

        // and list the dead properties as well.
        if (self.name == "propname" || self.name == "allprop") && self.fs.have_props(path).await {
            if let Ok(v) = self.fs.get_props(path, do_content).await {
                v.into_iter()
                    .map(davprop_to_element)
                    .for_each(|e| add_sc_elem(&mut props, StatusCode::OK, e));
            }
        }

        Ok::<(), DavError>(self.write_propresponse(path, props)?)
    }

    pub fn write_propresponse(
        &mut self,
        path: &DavPath,
        props: HashMap<StatusCode, Vec<Element>>,
    ) -> Result<(), DavError>
    {
        self.emitter.write(XmlWEvent::start_element("D:response"))?;
        let p = path.with_prefix().as_url_string();
        Element::new2("D:href").text(p).write_ev(&mut self.emitter)?;

        let mut keys = props.keys().collect::<Vec<_>>();
        keys.sort();
        for status in keys {
            let v = props.get(status).unwrap();
            self.emitter.write(XmlWEvent::start_element("D:propstat"))?;
            self.emitter.write(XmlWEvent::start_element("D:prop"))?;
            for i in v.iter() {
                i.write_ev(&mut self.emitter)?;
            }
            self.emitter.write(XmlWEvent::end_element())?;
            Element::new2("D:status")
                .text("HTTP/1.1 ".to_string() + &status.to_string())
                .write_ev(&mut self.emitter)?;
            self.emitter.write(XmlWEvent::end_element())?;
        }

        self.emitter.write(XmlWEvent::end_element())?; // response

        Ok(())
    }

    pub async fn flush(&mut self) -> DavResult<()> {
        let buffer = self.emitter.inner_mut().take();
        self.tx.as_mut().unwrap().send(Bytes::from(buffer)).await;
        Ok(())
    }

    pub async fn close(&mut self) -> DavResult<()> {
        let _ = self.emitter.write(XmlWEvent::end_element());
        self.flush().await
    }
}

fn add_sc_elem(hm: &mut HashMap<StatusCode, Vec<Element>>, sc: StatusCode, e: Element) {
    if !hm.contains_key(&sc) {
        hm.insert(sc, Vec::new());
    }
    hm.get_mut(&sc).unwrap().push(e)
}

fn element_to_davprop_full(elem: &Element) -> DavProp {
    let mut emitter = EventWriter::new(Cursor::new(Vec::new()));
    elem.write_ev(&mut emitter).ok();
    let xml = emitter.into_inner().into_inner();
    DavProp {
        name:      elem.name.clone(),
        prefix:    elem.prefix.clone(),
        namespace: elem.namespace.clone(),
        xml:       Some(xml),
    }
}

fn element_to_davprop(elem: &Element) -> DavProp {
    DavProp {
        name:      elem.name.clone(),
        prefix:    elem.prefix.clone(),
        namespace: elem.namespace.clone(),
        xml:       None,
    }
}

fn davprop_to_element(prop: DavProp) -> Element {
    if let Some(xml) = prop.xml {
        return Element::parse2(Cursor::new(xml)).unwrap();
    }
    let mut elem = Element::new(&prop.name);
    if let Some(ref ns) = prop.namespace {
        let pfx = prop.prefix.as_ref().map(|p| p.as_str()).unwrap_or("");
        elem = elem.ns(pfx, ns.as_str());
    }
    elem.prefix = prop.prefix;
    elem.namespace = prop.namespace.clone();
    elem
}
