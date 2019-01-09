
use std::io::{Cursor, Read};
use std::time::Duration;
use std::cmp;


use http::StatusCode as SC;
use xmltree::{self, Element};

use crate::xmltree_ext::{self, ElementExt};
use crate::ls::*;

use crate::errors::DavError;
use crate::headers::{self,Depth,Timeout,DavTimeout};
use crate::typed_headers::HeaderMapExt;
use crate::fs::{OpenOptions,FsError};
use crate::conditional::{if_match,dav_if_match};
use crate::webpath::WebPath;
use crate::{daverror,statuserror,fserror};
use crate::sync_adapter::{Request,Response};

impl crate::DavInner {
    pub(crate) fn handle_lock(&self, mut req: Request, mut res: Response) -> Result<(), DavError> {

        // read request.
        let mut xmldata = Vec::with_capacity(4096);
        req.read_to_end(&mut xmldata)?;

        // must have a locksystem or bail
        let locksystem = match self.ls {
            Some(ref ls) => ls,
            None => return Err(statuserror(&mut res, SC::METHOD_NOT_ALLOWED)),
        };

        // path and meta
        let (path, meta) = match self.fixpath(&req, &mut res) {
            Ok((path, meta)) => (path, Some(meta)),
            Err(_) => (self.path(&req), None),
        };

        // lock refresh?
        if xmldata.len() == 0 {

            // get locktoken
            let (_, tokens) = dav_if_match(&req, &self.fs, &self.ls, &path);
            if tokens.len() != 1 {
                return Err(statuserror(&mut res, SC::BAD_REQUEST));
            }

            // try refresh
            // FIXME: you can refresh a lock owned by someone else. is that OK?
            let timeout = get_timeout(&req, true, false);
            let lock = match locksystem.refresh(&path, &tokens[0], timeout) {
                Ok(lock) => lock,
                Err(_) => return Err(statuserror(&mut res, SC::PRECONDITION_FAILED)),
            };

            // output result
            let prop = build_lock_prop(&lock, true);
            *res.status_mut() = SC::OK;
            let res = res.start();
            let mut emitter = xmltree_ext::emitter(res)?;
            prop.write_ev(&mut emitter)?;

            return Ok(());
        }

        // handle Depth:
        let deep = match req.headers.typed_get::<Depth>() {
            Some(Depth::Infinity) | None => true,
            Some(Depth::Zero)=> false,
            _ => return Err(statuserror(&mut res, SC::BAD_REQUEST)),
        };

        // handle the if-headers.
        if let Some(s) = if_match(&req, meta.as_ref(), &self.fs, &self.ls, &path) {
            return Err(statuserror(&mut res, s));
        }

        // Cut & paste from method_put.rs ....
        let mut oo = OpenOptions::write();
        oo.create = true;
        if req.headers.typed_get::<headers::IfMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create = false;
        }
        if req.headers.typed_get::<headers::IfNoneMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create_new = true;
        }

        // parse xml
        let tree = xmltree::Element::parse2(Cursor::new(xmldata))
                .map_err(|e| daverror(&mut res, e))?;
        if tree.name != "lockinfo" {
            return Err(daverror(&mut res, DavError::XmlParseError));
        }

        // decode Element.
        let mut shared : Option<bool> = None;
        let mut owner : Option<Element> = None;
        let mut locktype = false;

        for elem in tree.children {
            match elem.name.as_str() {
                "lockscope" if elem.children.len() == 1 => {
                    match elem.children[0].name.as_ref() {
                        "exclusive" => shared = Some(false),
                        "shared" => shared = Some(true),
                        _ => return Err(DavError::XmlParseError),
                    }
                },
                "locktype" if elem.children.len() == 1 => {
                    match elem.children[0].name.as_ref() {
                        "write" => locktype = true,
                        _ => return Err(DavError::XmlParseError),
                    }
                },
                "owner" => {
                    let mut o = elem.clone();
                    o.prefix = Some("D".to_owned());
                    owner = Some(o);
                },
                _ => return Err(DavError::XmlParseError),
            }
        }

        // sanity check.
        if !shared.is_some() || !locktype {
            return Err(DavError::XmlParseError);
        };
        let shared = shared.unwrap();

        // create lock
        let timeout = get_timeout(&req, false, shared);
        let principal = self.principal.as_ref().map(|s| s.as_str());
        let lock = match locksystem.lock(&path, principal, owner.as_ref(), timeout, shared, deep) {
            Ok(lock) => lock,
            Err(_) => return Err(statuserror(&mut res, SC::LOCKED)),
        };

        // try to create file if it doesn't exist.
        if let None = meta {

            match self.fs.open(&path, oo) {
                Ok(_) => {},
                Err(FsError::NotFound) |
                Err(FsError::Exists) => {
                    let s = if !oo.create || oo.create_new {
                        SC::PRECONDITION_FAILED
                    } else {
                        SC::CONFLICT
                    };
                    locksystem.unlock(&path, &lock.token).ok();
                    return Err(statuserror(&mut res, s));
                },
                Err(e) => {
                    locksystem.unlock(&path, &lock.token).ok();
                    return Err(fserror(&mut res, e));
                },
            };
        }

        // output result
        res.headers_mut().typed_insert(headers::LockToken("<".to_string() + &lock.token + ">"));
        if let None = meta {
            *res.status_mut() = SC::CREATED;
        } else {
            *res.status_mut() = SC::OK;
        }

        let res = res.start();
        let mut emitter = xmltree_ext::emitter(res)?;
        let prop = build_lock_prop(&lock, true);
        prop.write_ev(&mut emitter)?;

        Ok(())
    }

    pub(crate) fn handle_unlock(&self, req: Request, mut res: Response) -> Result<(), DavError> {

        // must have a locksystem or bail
        let locksystem = match self.ls {
            Some(ref ls) => ls,
            None => return Err(statuserror(&mut res, SC::METHOD_NOT_ALLOWED)),
        };

        // Must have Lock-Token header
        let t = req.headers.typed_get::<headers::LockToken>()
            .ok_or(statuserror(&mut res, SC::BAD_REQUEST))?;
        let token = t.0.trim_matches(|c| c == '<' || c == '>');

        let (path, _) = self.fixpath(&req, &mut res).map_err(|e| fserror(&mut res, e))?;

        match locksystem.unlock(&path, token) {
            Ok(_) => {
                *res.status_mut() = SC::NO_CONTENT;
                Ok(())
            },
            Err(_) => {
                Err(statuserror(&mut res, SC::CONFLICT))
            }
        }
    }
}

pub(crate) fn list_lockdiscovery(ls: Option<&Box<DavLockSystem>>, path: &WebPath) -> Element {
    let mut elem = Element::new2("D:lockdiscovery");

    // must have a locksystem or bail
    let locksystem = match ls {
        Some(ls) => ls,
        None => return elem,
    };

    // list the locks.
    let locks = locksystem.discover(path);
    for lock in &locks {
        elem.push(build_lock_prop(lock, false));
    }
    elem
}

pub(crate) fn list_supportedlock(ls: Option<&Box<DavLockSystem>>) -> Element {
    let mut elem = Element::new2("D:supportedlock");

    // must have a locksystem or bail
    if ls.is_none() {
        return elem;
    }

    let mut entry = Element::new2("D:lockentry");
    let mut scope = Element::new2("D:lockscope");
    scope.push(Element::new2("D:exclusive"));
    scope.push(Element::new2("D:write"));
    entry.push(scope);
    elem.push(entry);

    let mut entry = Element::new2("D:lockentry");
    let mut scope = Element::new2("D:lockscope");
    scope.push(Element::new2("D:shared"));
    scope.push(Element::new2("D:write"));
    entry.push(scope);
    elem.push(entry);

    elem
}

// process timeout header
fn get_timeout(req: &Request, refresh: bool, shared: bool) -> Option<Duration> {
    let max_timeout = if shared {
        Duration::new(86400, 0)
    } else {
        Duration::new(600, 0)
    };
    match req.headers.typed_get::<Timeout>() {
        Some(headers::Timeout(ref vec)) if vec.len() > 0 => {
            match vec[0] {
                DavTimeout::Infinite => {
                    if refresh { None } else { Some(max_timeout) }
                },
                DavTimeout::Seconds(n) => Some(cmp::min(max_timeout, Duration::new(n as u64, 0))),
            }
        }
        _ => None,
    }
}

fn build_lock_prop(lock: &DavLock, full: bool) -> Element {
    let mut actlock = Element::new2("D:activelock");

    let mut elem = Element::new2("D:lockscope");
    elem.push(match lock.shared {
        false	=> Element::new2("D:exclusive"),
        true	=> Element::new2("D:shared"),
    });
    actlock.push(elem);

    let mut elem = Element::new2("D:locktype");
    elem.push(Element::new2("D:write"));
    actlock.push(elem);

    actlock.push(Element::new2("D:depth").text(match lock.deep {
        false	=> "0",
        true	=> "Infinity",
    }.to_string()));

    actlock.push(Element::new2("D:timeout").text(match lock.timeout {
		None => "Infinite".to_string(),
		Some(d) => format!("Second-{}", d.as_secs()),
	}));
    let mut locktokenelem = Element::new2("D:locktoken");
    locktokenelem.push(Element::new2("D:href").text(lock.token.clone()));
    actlock.push(locktokenelem);

    let mut lockroot = Element::new2("D:lockroot");
    lockroot.push(Element::new2("D:href").text(lock.path.as_url_string_with_prefix()));
    actlock.push(lockroot);

    if let Some(ref o) = lock.owner {
        actlock.push(o.clone());
    }

    if !full {
        return actlock;
    }

    let mut ldis = Element::new2("D:lockdiscovery");
    ldis.push(actlock);
    let mut prop = Element::new2("D:prop").ns("D", "DAV:");
    prop.push(ldis);

	prop
}

