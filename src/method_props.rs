
use std;
use std::io::{Cursor,Write};
use std::io::BufWriter;
use std::borrow::Cow;
use std::collections::HashMap;

use hyper;
use hyper::status::StatusCode as SC;
use hyper::server::{Request,Response};
use hyper::net::Streaming;

use xml::EmitterConfig;
use xml::common::XmlVersion;
use xml::writer::EventWriter;
use xml::writer::XmlEvent as XmlWEvent;

use xmltree::Element;
use xmltree_ext::*;

use fserror;
use headers;
use webpath::*;
use fs::*;

use conditional::if_match;
use errors::DavError;
use fserror_to_status;

use method_lock::{list_lockdiscovery,list_supportedlock};
use {DavHandler,DavResult};
use {systemtime_to_httpdate,systemtime_to_rfc3339};
use {daverror,statuserror};

const NS_APACHE_URI: &'static str = "http://apache.org/dav/props/";
const NS_DAV_URI: &'static str = "DAV:";
const NS_XS4ALL_URI: &'static str = "http://xs4all.net/dav/props/";
const NS_MS_URI: &'static str = "urn:schemas-microsoft-com:";

const PROPNAME_STR: &'static [&'static str] = &[
    "D:creationdate", "D:displayname", "D:getcontentlanguage",
    "D:getcontentlength", "D:getcontenttype", "D:getetag", "D:getlastmodified",
    "D:lockdiscovery", "D:resourcetype", "D:supportedlock",
    "D:quota-available-bytes", "D:quota-used-bytes",
    "A:executable", "X:atime", "X:ctime", "M:Win32LastAccessTime"
];

const ALLPROP_STR: &'static [&'static str] = &[
    "D:creationdate", "D:displayname", "D:getcontentlanguage",
    "D:getcontentlength", "D:getcontenttype", "D:getetag", "D:getlastmodified",
    "D:lockdiscovery", "D:resourcetype", "D:supportedlock",
];

lazy_static! {
    static ref ALLPROP : Vec<Element> = init_staticprop(ALLPROP_STR);
    static ref PROPNAME : Vec<Element> = init_staticprop(PROPNAME_STR);
}

type Emitter<'a> = EventWriter<BufWriter<Response<'a, Streaming>>>;

struct PropWriter<'a, 'k: 'a> {
    emitter:    Emitter<'k>,
    name:       &'a str,
    props:      Vec<Element>,
    fs:         &'a Box<DavFileSystem>,
    useragent:  &'a str,
    q_cache:    QuotaCache,
}

#[derive(Default,Clone,Copy)]
struct QuotaCache {
    q_state:    u32,
    q_used:     u64,
    q_total:    Option<u64>,
}

fn init_staticprop(p: &[&str]) -> Vec<Element> {
    let mut v = Vec::new();
    for a in p {
        let mut e = Element::new2(*a);
        e.namespace = match e.prefix.as_ref().map(|x| x.as_str()) {
            Some("D") => Some(NS_DAV_URI.to_string()),
            Some("A") => Some(NS_APACHE_URI.to_string()),
            Some("X") => Some(NS_XS4ALL_URI.to_string()),
            Some("M") => Some(NS_MS_URI.to_string()),
            _ => None,
        };
        v.push(e);
    }
    v
}

impl DavHandler {

    pub(crate) fn handle_propfind(&self, mut req: Request, mut res: Response) -> DavResult<()> {

        // No checks on If: and If-* headers here, because I do not see
        // the point and there's nothing in RFC4918 that indicates we should.

        let xmldata = self.read_request_max(&mut req, 8192);

        let cc = vec!(b"no-store, no-cache, must-revalidate".to_vec());
        let pg = vec!(b"no-cache".to_vec());
        res.headers_mut().set_raw("Cache-Control", cc);
        res.headers_mut().set_raw("Pragma", pg);

        let depth = match req.headers.get::<headers::Depth>() {
            Some(&headers::Depth::Infinity) | None => {
                if let None = req.headers.get::<headers::XLitmus>() {
                    *res.status_mut() = SC::Forbidden;
                    write!(res.start()?, "PROPFIND requests with a Depth of \"infinity\" are not allowed\r\n")?;
                    return Err(DavError::Status(SC::Forbidden));
                }
                headers::Depth::Infinity
            },
            Some(d) => d.clone(),
        };

        let (path, meta) = self.fixpath(&req, &mut res).map_err(|e| fserror(&mut res, e))?;

        let mut root = None;
        if xmldata.len() > 0 {
            root = match Element::parse(Cursor::new(xmldata)) {
                Ok(t) => {
                    if t.name == "propfind" &&
                       t.namespace == Some("DAV:".to_owned()) {
                           Some(t)
                    } else {
                        return Err(daverror(&mut res, DavError::XmlParseError));
                    }
                },
                // Err(e) => return Err(daverror(&mut res, e)),
                Err(_) => return Err(daverror(&mut res, DavError::XmlParseError)),
            };
        }

        let (name, props) = match root {
            None => ("allprop", Vec::new()),
            Some(mut elem) => {
                let includes = elem.take_child("includes").map_or(Vec::new(), |c| c.children);
                match elem.children.iter()
                    .position(|e| e.name == "propname" || e.name == "prop" || e.name == "allprop")
                    .map(|i| elem.children.remove(i)) {
                    Some(elem) => {
                        match elem.name.as_str() {
                            "propname" => ("propname", Vec::new()),
                            "prop" => ("prop", elem.children),
                            "allprop" => ("allprop", includes),
                            _ => return Err(daverror(&mut res, DavError::XmlParseError)),
                        }
                    },
                    None => return Err(daverror(&mut res, DavError::XmlParseError)),
                }
            }
        };

        debug!("propfind: type request: {}", name);

        let mut pw = PropWriter::new(&req, res, name, props, &self.fs)?;
        pw.write_props(&path, meta.as_ref())?;

        if meta.is_dir() && depth != headers::Depth::Zero {
            self.propfind_directory(&path, depth, &mut pw)?;
        }
        pw.close()?;

        Ok(())
    }

    fn propfind_directory(&self, path: &WebPath, depth: headers::Depth, propwriter: &mut PropWriter) -> DavResult<()> {
        let entries = match self.fs.read_dir(path) {
            Ok(entries) => entries,
            Err(e) => { error!("read_dir error {:?}", e); return Ok(()); },
        };
        for dirent in entries {
            let mut npath = path.clone();
            npath.push_segment(&dirent.name());
            let meta = match self.fs.metadata(&npath) {
                Ok(meta) => meta,
                Err(e) => {
                    debug!("metadata error on {}. Skipping {:?}", npath, e);
                    continue;
                }
            };
            if meta.is_dir() {
                npath.add_slash();
            }
            propwriter.write_props(&npath, meta.as_ref())?;
            if depth == headers::Depth::Infinity && meta.is_dir() {
                self.propfind_directory(&npath, depth, propwriter)?;
            }
        }
        Ok(())
    }

    // set/change a live property. returns StatusCode::Continue if
    // this wasnt't  a live property (or, if we want it handled
    // as a dead property, e.g. DAV:displayname).
    fn liveprop_set(&self, prop: &Element, can_deadprop: bool) -> SC {
        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => match prop.name.as_str() {
                "getcontentlanguage" => {
                    if prop.text.is_none() || prop.children.len() > 0 {
                        return SC::Conflict;
                    }
                    // only here to make "litmus" happy, really...
                    if let Some(ref s) = prop.text {
                        use hyper::header::Header;
                        use hyper::header::ContentLanguage;
                        match ContentLanguage::parse_header(&[s.as_bytes().to_vec()]) {
                            Ok(ContentLanguage(ref v)) if v.len() > 0 => {},
                            _ => return SC::Conflict,
                        }
                    }
                    if can_deadprop { SC::Continue } else { SC::Forbidden }
                },
                "displayname" => {
                    if prop.text.is_none() || prop.children.len() > 0 {
                        return SC::Conflict;
                    }
                    if can_deadprop { SC::Continue } else { SC::Forbidden }
                },
                "getlastmodified" => {
                    // we might allow setting modified time
                    // by using utimes() on unix. Not yet though.
                    if prop.text.is_none() || prop.children.len() > 0 {
                        return SC::Conflict;
                    }
                    SC::Forbidden
                },
                _ => SC::Forbidden,
            },
            Some(NS_APACHE_URI) => match prop.name.as_str() {
                "executable" => {
                    // we could allow toggling the execute bit.
                    // to be implemented.
                    if prop.text.is_none() || prop.children.len() > 0 {
                        return SC::Conflict;
                    }
                    SC::Forbidden
                },
                _ => SC::Forbidden,
            },
            Some(NS_XS4ALL_URI) => {
                // no xs4all properties can be changed.
                SC::Forbidden
            },
            Some(NS_MS_URI) => match prop.name.as_str() {
                "Win32CreationTime" |
                "Win32FileAttributes" |
                "Win32LastAccessTime" |
                "Win32LastModifiedTime" => {
                    if prop.text.is_none() || prop.children.len() > 0 {
                        return SC::Conflict;
                    }
                    // Always report back that we successfully
                    // changed these, even if we didn't --
                    // makes the windows webdav client work.
                    SC::Ok
                },
                _ => SC::Forbidden,
            },
            _ => SC::Continue,
        }
    }

    // In general, live properties cannot be removed, with the
    // exception of getcontentlanguage and displayname.
    fn liveprop_remove(&self, prop: &Element, can_deadprop: bool) -> SC {
        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => match prop.name.as_str() {
                "getcontentlanguage" |
                "displayname" => {
                    if can_deadprop {
                        SC::Ok
                    } else {
                        SC::Forbidden
                    }
                },
                _ => SC::Forbidden,
            },
            Some(NS_APACHE_URI) |
            Some(NS_XS4ALL_URI) |
            Some(NS_MS_URI) => SC::Forbidden,
            _ => SC::Continue,
        }
    }

    pub(crate) fn handle_proppatch(&self, mut req: Request, mut res: Response) -> Result<(), DavError> {

        // read request.
        let xmldata = self.read_request_max(&mut req, 65536);

        // file must exist.
        let (path, meta) = match self.fixpath(&req, &mut res) {
            Ok((path, meta)) => (path, meta),
            Err(e) => return Err(fserror(&mut res, e)),
        };

        // handle the if-headers.
        if let Some(s) = if_match(&req, Some(&meta), &self.fs, &path) {
            return Err(statuserror(&mut res, s));
        }

        debug!(target: "xml", "proppatch input:\n{}]\n",
               std::string::String::from_utf8_lossy(&xmldata));

        // parse xml
        let tree = Element::parse2(Cursor::new(xmldata))
                .map_err(|e| daverror(&mut res, e))?;
        if tree.name != "propertyupdate" {
            return Err(daverror(&mut res, DavError::XmlParseError));
        }

        let mut set = Vec::new();
        let mut rem = Vec::new();
        let mut ret = Vec::new();
        let can_deadprop = self.fs.have_props(&path);

        // walk over the element tree and feed "set" and "remove" items to
        // the liveprop_set/liveprop_remove functions. If skipped by those,
        // gather them in the set/rem Vec to be processed as dead properties.
        for mut elem in &tree.children {
            for n in elem.children.iter()
                        .filter(|f| f.name == "prop")
                        .flat_map(|f| &f.children) {
                match elem.name.as_str() {
                    "set" => {
                        match self.liveprop_set(&n, can_deadprop) {
                            SC::Continue => set.push(element_to_davprop_full(&n)),
                            s => ret.push((s, element_to_davprop(&n))),
                        }
                    },
                    "remove" => {
                        match self.liveprop_remove(&n, can_deadprop) {
                            SC::Continue => rem.push(element_to_davprop(&n)),
                            s => ret.push((s, element_to_davprop(&n))),
                        }
                    },
                    _ => {},
                }
            }
        }

        // if any set/remove failed, stop processing here.
        if ret.iter().any(|&(ref s, _)| s != &SC::Ok) {
            ret = ret.into_iter().map(|(s, p)|
                if s == SC::Ok {
                    (SC::FailedDependency, p)
                } else {
                    (s, p)
                }
            ).collect::<Vec<_>>();
            ret.extend(set.into_iter().chain(rem.into_iter())
                    .map(|p| (SC::FailedDependency, p)));
        } else if set.len() > 0 || rem.len() > 0 {
            // hmmm ... we assume nothing goes wrong here at the
            // moment. if it does, we should roll back the earlier
            // made changes to live props, but come on, we're not
            // builing a transaction engine here.
            let deadret = self.fs.patch_props(&path, set, rem)
                .map_err(|e| DavError::Status(fserror_to_status(e)))?;
            ret.extend(deadret.into_iter());
        }

        // group by statuscode.
        let mut hm = HashMap::new();
        for (code, prop) in ret.into_iter() {
           if !hm.contains_key(&code) {
               hm.insert(code, Vec::new());
            }
            let mut v = hm.get_mut(&code).unwrap();
            v.push(davprop_to_element(prop));
        }

        // And reply.
        let mut pw = PropWriter::new(&req, res, "propertyupdate", Vec::new(), &self.fs)?;
        pw.write_propresponse(&path, hm)?;
        pw.close()?;

        Ok(())
    }
}

impl<'a, 'k> PropWriter<'a, 'k> {

    pub fn new(req: &'a Request, mut res: Response<'k>, name: &'a str, mut props: Vec<Element>, fs: &'a Box<DavFileSystem>) -> DavResult<PropWriter<'a, 'k>> {

        let contenttype = vec!(b"application/xml; charset=utf-8".to_vec());
        res.headers_mut().set_raw("Content-Type", contenttype);
        *res.status_mut() = SC::MultiStatus;
        let res = res.start()?;

        let mut emitter = EventWriter::new_with_config(
                              BufWriter::new(res),
                              EmitterConfig {
                                  normalize_empty_elements: false,
                                  perform_indent: false,
                                  indent_string: Cow::Borrowed(""),
                                  ..Default::default()
                              }
                          );
        emitter.write(XmlWEvent::StartDocument {
                      version: XmlVersion::Version10,
                      encoding: Some("utf-8"),
                      standalone: None,
                })?;

        let mut ev = XmlWEvent::start_element("D:multistatus").ns("D", NS_DAV_URI);

        // could check the prop prefixes and namespace to see if we
        // actually need these.
        if name != "propertyupdate" {
            ev = ev.ns("A", NS_APACHE_URI).ns("X", NS_XS4ALL_URI).ns("M", NS_MS_URI);
        }

        emitter.write(ev)?;

        if name != "prop" && name != "propertyupdate" {
            let mut v = Vec::new();
            let iter = if name == "allprop" { ALLPROP.iter() } else { PROPNAME.iter() };
            for a in iter {
                if !props.iter().any(|e| a.namespace == e.namespace && a.name == e.name) {
                    v.push(a.clone());
                }
            }
            props.append(&mut v);
        }

        let ua = match req.headers.get::<hyper::header::UserAgent>() {
            Some(s) => &s.0,
            None => "",
        };

        Ok(PropWriter {
            emitter:    emitter,
            name:       name,
            props:      props,
            fs:         fs,
            useragent:  ua,
            q_cache:    Default::default(),
        })
    }

    fn build_elem<'b, T>(&self, content: bool, e: &Element, text: T) -> (SC, Element)
            where T: Into<Cow<'a, str>> {
        let mut e = e.clone();
        if content {
            let t = text.into();
            if t != "" {
                e.text = Some(t.to_string());
            }
        }
        (SC::Ok, e)
    }

    fn get_quota(&self, qc: &mut QuotaCache, path: &WebPath, meta: &DavMetaData) -> FsResult<(u64, Option<u64>)> {
        // do lookup only once.
        match qc.q_state {
            0 => {
                match self.fs.get_quota() {
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

    fn build_prop(&self, prop: &Element, path: &WebPath, meta: &DavMetaData, mut qc: &mut QuotaCache, docontent: bool) -> (SC, Element) {

        // in some cases, a live property might be stored in the
        // dead prop database, like DAV:displayname.
        let mut try_deadprop = false;

        match prop.namespace.as_ref().map(|x| x.as_str()) {
            Some(NS_DAV_URI) => match prop.name.as_str() {
                "creationdate" => {
                    if let Ok(time) = meta.created() {
                        let tm = systemtime_to_rfc3339(time);
                        return self.build_elem(docontent, prop, tm);
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
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                "displayname" |
                "getcontentlanguage" => {
                    try_deadprop = true;
                },
                "getetag" => {
                    return self.build_elem(docontent, prop, meta.etag());
                },
                "getcontentlength" => {
                    if !meta.is_dir() {
                        return self.build_elem(docontent, prop, meta.len().to_string());
                    }
                },
                "getcontenttype" => {
                    return if meta.is_dir() {
                        self.build_elem(docontent, prop, "httpd/unix-directory")
                    } else {
                        self.build_elem(docontent, prop, path.get_mime_type_str())
                    };
                },
                "getlastmodified" => {
                    if let Ok(time) = meta.modified() {
                        let tm = format!("{}", systemtime_to_httpdate(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                "resourcetype" => {
                    let mut elem = prop.clone();
                    if meta.is_dir() && docontent {
                        let dir = Element::new2("D:collection");
                        elem.children.push(dir);
                    }
                    return (SC::Ok, elem);
                },
                "supportedlock" => {
                    return (SC::Ok, list_supportedlock());
                },
                "lockdiscovery" => {
                    return (SC::Ok, list_lockdiscovery());
                },
                "quota-available-bytes" => {
                    if let Ok((_, Some(avail))) = self.get_quota(&mut qc, path, meta) {
                        return self.build_elem(docontent, prop, avail.to_string());
                    }
                },
                "quota-used-bytes" => {
                    if let Ok((used, _)) = self.get_quota(&mut qc, path, meta) {
                        let used = if self.useragent.contains("WebDAVFS") {
                            // Need this on OSX, otherwise the value is off
                            // by a factor of 10 or so .. ?!?!!?
                            format!("{:014}", used)
                        } else {
                            used.to_string()
                        };
                        return self.build_elem(docontent, prop, used);
                    }
                },
                _ => {},
            },
            Some(NS_APACHE_URI) => match prop.name.as_str() {
                "executable" => {
                    if let Ok(x) = meta.executable() {
                        let b = if x { "T" } else { "F" };
                        return self.build_elem(docontent, prop, b);
                    }
                },
                _ => {},
            },
            Some(NS_XS4ALL_URI) => match prop.name.as_str() {
                "atime" => {
                    if let Ok(time) = meta.accessed() {
                        let tm = format!("{}", systemtime_to_rfc3339(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                "ctime" => {
                    if let Ok(time) = meta.status_changed() {
                        let tm = systemtime_to_rfc3339(time);
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                _ => {},
            },
            Some(NS_MS_URI) => match prop.name.as_str() {
                "Win32CreationTime" => {
                    if let Ok(time) = meta.created() {
                        let tm = format!("{}", systemtime_to_httpdate(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                    // use ctime instead - apache seems to do this.
                    if let Ok(ctime) = meta.status_changed() {
                        let mut time = ctime;
                        if let Ok(mtime) = meta.modified() {
                            if mtime < ctime {
                                time = mtime;
                            }
                        }
                        let tm = format!("{}", systemtime_to_httpdate(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                "Win32LastAccessTime" => {
                    if let Ok(time) = meta.accessed() {
                        let tm = format!("{}", systemtime_to_httpdate(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                "Win32LastModifiedTime" => {
                    if let Ok(time) = meta.modified() {
                        let tm = format!("{}", systemtime_to_httpdate(time));
                        return self.build_elem(docontent, prop, tm);
                    }
                },
                _ => {},
            },
            _ => {
                try_deadprop = true;
            },
        }

        if try_deadprop && self.name == "prop" && self.fs.have_props(path) {
            // asking for a specific property.
            let dprop = element_to_davprop(prop);
            if let Ok(xml) = self.fs.get_prop(path, dprop) {
                if let Ok(e) = Element::parse(Cursor::new(xml)) {
                    return (SC::Ok, e);
                }
            }
        }
        (SC::NotFound, prop.clone())
    }

    fn write_props(&mut self, path: &WebPath, meta: &DavMetaData) -> Result<(), DavError> {

        // A HashMap<StatusCode, Vec<Element>> for the result.
        let mut props = HashMap::new();
        fn add_sc_elem(hm: &mut HashMap<SC, Vec<Element>>, sc: SC, e: Element) {
            if !hm.contains_key(&sc) {
                hm.insert(sc, Vec::new());
            }
            hm.get_mut(&sc).unwrap().push(e)
        }

        // Get properties one-by-one
        let do_content = self.name != "propname";
        let mut qc = self.q_cache;
        for mut p in &self.props {
            let (sc, elem) = self.build_prop(p, path, meta, &mut qc, do_content);
            if sc == SC::Ok || (self.name != "propname" && self.name != "allprop") {
                add_sc_elem(&mut props, sc, elem);
            }
        }
        self.q_cache = qc;

        // and list the dead properties as well.
        if (self.name == "propname" || self.name == "allprop") && self.fs.have_props(path) {
            if let Ok(v) = self.fs.get_props(path, do_content) {
                v.into_iter().map(davprop_to_element).
                    for_each(|e| add_sc_elem(&mut props, SC::Ok, e));
            }
        }

        self.write_propresponse(path, props)
    }

    fn write_propresponse(&mut self, path: &WebPath, props: HashMap<SC, Vec<Element>>) -> Result<(), DavError> {

        self.emitter.write(XmlWEvent::start_element("D:response"))?;
        let p = path.as_url_string_with_prefix();
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
            Element::new2("D:status").text("HTTP/1.1 ".to_string() + &status.to_string()).write_ev(&mut self.emitter)?;
            self.emitter.write(XmlWEvent::end_element())?;
        }

        self.emitter.write(XmlWEvent::end_element())?; // response

        Ok(())
    }

    pub(crate) fn close(mut self) -> Result<(), DavError> {
        self.emitter.write(XmlWEvent::end_element())?;
        self.emitter.into_inner().flush()?;
        Ok(())
    }

}

fn element_to_davprop_full(elem: &Element) -> DavProp {
    let mut emitter = EventWriter::new(Cursor::new(Vec::new()));
    elem.write_ev(&mut emitter).ok();
    let xml = emitter.into_inner().into_inner();
    DavProp{
        name:       elem.name.clone(),
        prefix:     elem.prefix.clone(),
        namespace:  elem.namespace.clone(),
        xml:        Some(xml),
    }
}

fn element_to_davprop(elem: &Element) -> DavProp {
    DavProp{
        name:       elem.name.clone(),
        prefix:     elem.prefix.clone(),
        namespace:  elem.namespace.clone(),
        xml:        None,
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

