
use std;
use std::io::{Cursor,Write};
use std::io::BufWriter;
use std::borrow::Cow;
use std::collections::HashMap;

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

use conditional::ifmatch;
use errors::DavError;
use fserror_to_status;

use method_lock::{list_lockdiscovery,list_supportedlock};
use {DavHandler,DavResult};
use {systemtime_to_httpdate,systemtime_to_rfc3339};
use {daverror,statuserror};

const NS_APACHE_URI: &'static str = "http://apache.org/dav/props/";
const NS_DAV_URI: &'static str = "DAV:";
const NS_XS4ALL_URI: &'static str = "http://xs4all.net/dav/props/";

const ALLPROP_STR: &'static [&'static str] = &[
    "D:creationdate", "D:displayname", "D:getcontentlanguage",
    "D:getcontentlength", "D:getcontenttype", "D:getetag", "D:getlastmodified",
    "D:lockdiscovery", "D:resourcetype", "D:supportedlock",
    "A:executable", "X:atime", "X:ctime",
];

lazy_static! {
    static ref ALLPROP : Vec<Element> = {
        let mut v = Vec::new();
        for a in ALLPROP_STR {
            let mut e = Element::new2(*a);
            e.namespace = match e.prefix.as_ref().map(|x| x.as_str()) {
                Some("D") => Some(NS_DAV_URI.to_string()),
                Some("A") => Some(NS_APACHE_URI.to_string()),
                Some("X") => Some(NS_XS4ALL_URI.to_string()),
                _ => None,
            };
            v.push(e);
        }
        v
    };
}

type Emitter<'a> = EventWriter<BufWriter<Response<'a, Streaming>>>;

struct PropWriter<'a, 'k: 'a> {
    emitter:    Emitter<'k>,
    name:       &'a str,
    props:      Vec<Element>,
    fs:         &'a Box<DavFileSystem>,
}

impl DavHandler {

    pub(crate) fn handle_propfind(&self, mut req: Request, mut res: Response) -> DavResult<()> {

        let xmldata = self.read_request_max(&mut req, 8192);

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

        let mut pw = PropWriter::new(res, name, props, &self.fs)?;
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
            let meta = match dirent.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    error!("Metadata error on {:?}. Skipping {:?}", dirent, e);
                    continue;
                }
            };
            let mut npath = path.clone();
            npath.push_segment(&dirent.name());
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

    pub(crate) fn handle_proppatch(&self, mut req: Request, mut res: Response) -> Result<(), DavError> {

        // read request.
        let xmldata = self.read_request_max(&mut req, 65536);

        // file must exist.
        let (path, meta) = match self.fixpath(&req, &mut res) {
            Ok((path, meta)) => (path, meta),
            Err(e) => return Err(fserror(&mut res, e)),
        };

        // handle the if-headers.
        if let Some(s) = ifmatch(&req, Some(&meta)) {
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

        fn classify_props(elem: &Element) -> (Vec<DavProp>, Vec<DavProp>) {
            let mut normal = Vec::new();
            let mut protected = Vec::new();
            for n in elem.children.iter()
                        .filter(|f| f.name == "prop")
                        .flat_map(|f| &f.children)
                        .map(|p| element_to_davprop_full(&p)) {
                match n.namespace.as_ref().map(|x| x.as_str()) {
                    Some(NS_DAV_URI) |
                    Some(NS_APACHE_URI) |
                    Some(NS_XS4ALL_URI) => protected.push(n),
                    _ => normal.push(n)
                }
            }
            (normal, protected)
        }

        let mut set_normal = Vec::new();
        let mut rem_normal = Vec::new();
        let mut set_protected = Vec::new();
        let mut rem_protected = Vec::new();

        // decode Element.
        for mut elem in &tree.children {
            match elem.name.as_str() {
                "set" => {
                    let (normal, protected) = classify_props(elem);
                    set_normal.extend(normal);
                    set_protected.extend(protected);
                },
                "remove" => {
                    let (normal, protected) = classify_props(elem);
                    rem_normal.extend(normal);
                    rem_protected.extend(protected);
                },
                _ => {},
            }
        }

        let mut ret = Vec::<(SC, DavProp)>::new();

        // trying to set protected properties?
        if set_protected.len() > 0 || rem_protected.len() > 0 {
            // right now we fail them all, we might allow setting
            // some live properties later on.
            ret.extend(set_protected.into_iter().chain(rem_protected.into_iter())
                    .map(|p| (SC::Forbidden, p)));
            ret.extend(set_normal.into_iter().chain(rem_normal.into_iter())
                    .map(|p| (SC::FailedDependency, p)));
        } else {
            ret = self.fs.patch_props(&path, set_normal, rem_normal)
                .map_err(|e| DavError::Status(fserror_to_status(e)))?;
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

/*
        // Now we only support the Win32 attributes so that the
        // windows minidirector works. Support as in "we say we
        // succeeded but actually we don't do anything".
        // We could handle them like live properties, I guess.
        let mut p_ok = Vec::new();
        let mut p_failed = Vec::new();
        for v in set.into_iter().chain(rem) {
            match v.name.as_str() {
                "Win32CreationTime" |
                "Win32LastAccessTime" |
                "Win32LastModifiedTime" |
                "Win32FileAttributes" => {
                    p_ok.push(v);
                },
                _ => {
                    p_failed.push(v);
                }
            }
        }

        // If there were unsupported props, all must fail.
        let mut hm = HashMap::new();
        if p_failed.len() == 0 {
            hm.insert(SC::Ok, p_ok);
        } else {
            hm.insert(SC::Conflict, p_failed);
            if p_ok.len() > 0 {
                hm.insert(SC::FailedDependency, p_ok);
            }
        }
*/
        // And reply.
        let mut pw = PropWriter::new(res, "propertyupdate", Vec::new(), &self.fs)?;
        pw.write_proppatch(&path, hm)?;
        pw.close()?;

        Ok(())
    }
}

impl<'a, 'k> PropWriter<'a, 'k> {

    pub fn new(mut res: Response<'k>, name: &'a str, mut props: Vec<Element>, fs: &'a Box<DavFileSystem>) -> DavResult<PropWriter<'a, 'k>> {

        let contenttype = vec!(b"text/xml; charset=\"utf-8\"".to_vec());
        res.headers_mut().set_raw("Content-Type", contenttype);
        *res.status_mut() = SC::MultiStatus;
        let res = res.start()?;

        let mut emitter = EventWriter::new_with_config(
                              BufWriter::new(res),
                              EmitterConfig {
                                  perform_indent: true,
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
            ev = ev.ns("A", NS_APACHE_URI).ns("X", NS_XS4ALL_URI);
        }

        emitter.write(ev)?;

        if name != "prop" && name != "propertyupdate" {
            let mut v = Vec::new();
            for a in ALLPROP.iter() {
                if !props.iter().any(|e| a.namespace == e.namespace && a.name == e.name) {
                    v.push(a.clone());
                }
            }
            props.append(&mut v);
        }

        Ok(PropWriter {
            emitter:    emitter,
            name:       name,
            props:      props,
            fs:         fs,
        })
    }

    fn build_elem<'b, T>(&self, content: bool, e: &Element, text: T) -> (Element, bool)
            where T: Into<Cow<'a, str>> {
        let mut e = e.clone();
        if content {
            let t = text.into();
            if t != "" {
                e.text = Some(t.to_string());
            }
        }
        (e, true)
    }

    fn build_prop(&self, prop: &Element, path: &WebPath, meta: &DavMetaData, docontent: bool) -> (Element, bool) {
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
                    return (elem, true);
                },
                "supportedlock" => {
                    return (list_supportedlock(), true);
                }
                "lockdiscovery" => {
                    return (list_lockdiscovery(), true);
                }
                _ => {},
            },
            Some(NS_APACHE_URI) => match prop.name.as_str() {
                "executable" => {
                    if let Ok(x) = meta.executable() {
                        let b = if x { "T" } else { "F" };
                        return self.build_elem(docontent, prop, b);
                    }
                }
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
            _ if self.name == "prop" && self.fs.have_props(path) => {
                // asking for a specific property.
                let dprop = element_to_davprop(prop);
                if let Ok(xml) = self.fs.get_prop(path, dprop) {
                    if let Ok(e) = Element::parse(Cursor::new(xml)) {
                        return (e, true);
                    }
                }
            },
            _ => {},
        }
        (prop.clone(), false)
    }

    fn write_props(&mut self, path: &WebPath, meta: &DavMetaData) -> Result<(), DavError> {

        self.emitter.write(XmlWEvent::start_element("D:response"))?;
        let p = path.as_url_string();
        Element::new2("D:href").text(p).write_ev(&mut self.emitter)?;

        let mut found = Element::new2("D:prop");
        let mut notfound = Element::new2("D:prop");
        for mut p in &self.props {
            let (e, ok) = self.build_prop(p, path, meta, self.name != "propname");
            if ok {
                found.push(e);
            } else if self.name == "prop" {
                notfound.push(e);
            }
        }

        // and list the dead properties as well.
        if (self.name == "propname" || self.name == "allprop") && self.fs.have_props(path) {
            if let Ok(v) = self.fs.get_props(path, self.name != "propname") {
                let v = v.into_iter().map(davprop_to_element).collect::<Vec<Element>>();
                found.children.extend(v);
            }
        }

        if found.has_children() {
    	    self.emitter.write(XmlWEvent::start_element("D:propstat"))?;
            found.write_ev(&mut self.emitter)?;
            Element::new2("D:status").text("HTTP/1.1 200 OK").write_ev(&mut self.emitter)?;
            self.emitter.write(XmlWEvent::end_element())?;
        }

        if self.name == "prop" && notfound.has_children() {
    	    self.emitter.write(XmlWEvent::start_element("D:propstat"))?;
            notfound.write_ev(&mut self.emitter)?;
            Element::new2("D:status").text("HTTP/1.1 404 Not Found").write_ev(&mut self.emitter)?;
            self.emitter.write(XmlWEvent::end_element())?;
        }

        self.emitter.write(XmlWEvent::end_element())?; // response

        Ok(())
    }

    fn write_proppatch(&mut self, path: &WebPath, props: HashMap<SC, Vec<Element>>) -> Result<(), DavError> {

        self.emitter.write(XmlWEvent::start_element("D:response"))?;
        let p = path.as_url_string();
        Element::new2("D:href").text(p).write_ev(&mut self.emitter)?;

        for (status, v) in props {
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

