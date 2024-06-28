use std::cmp;
use std::io::Cursor;
use std::time::Duration;

use headers::HeaderMapExt;
use http::StatusCode as SC;
use http::{Request, Response};
use xmltree::{self, Element};

use crate::body::Body;
use crate::conditional::{dav_if_match, if_match};
use crate::davheaders::{self, DavTimeout};
use crate::davpath::DavPath;
use crate::errors::*;
use crate::fs::{FsError, OpenOptions};
use crate::ls::*;
use crate::util::MemBuffer;
use crate::xmltree_ext::{self, ElementExt};
use crate::{DavInner, DavResult};

impl<C: Clone + Send + Sync + 'static> DavInner<C> {
    pub(crate) async fn handle_lock(
        &self,
        req: &Request<()>,
        xmldata: &[u8],
    ) -> DavResult<Response<Body>> {
        // must have a locksystem or bail
        let locksystem = match self.ls {
            Some(ref ls) => ls,
            None => return Err(SC::METHOD_NOT_ALLOWED.into()),
        };

        let mut res = Response::new(Body::empty());

        // path and meta
        let mut path = self.path(req);
        let meta = match self.fs.metadata(&path, &self.credentials).await {
            Ok(meta) => Some(self.fixpath(&mut res, &mut path, meta)),
            Err(_) => None,
        };

        // lock refresh?
        if xmldata.is_empty() {
            // get locktoken
            let (_, tokens) =
                dav_if_match(req, self.fs.as_ref(), &self.ls, &path, &self.credentials).await;
            if tokens.len() != 1 {
                return Err(SC::BAD_REQUEST.into());
            }

            // try refresh
            // FIXME: you can refresh a lock owned by someone else. is that OK?
            let timeout = get_timeout(req, true, false);
            let lock = match locksystem.refresh(&path, &tokens[0], timeout) {
                Ok(lock) => lock,
                Err(_) => return Err(SC::PRECONDITION_FAILED.into()),
            };

            // output result
            let prop = build_lock_prop(&lock, true);
            let mut emitter = xmltree_ext::emitter(MemBuffer::new())?;
            prop.write_ev(&mut emitter)?;
            let buffer = emitter.into_inner().take();

            let ct = "application/xml; charset=utf-8".to_owned();
            res.headers_mut().typed_insert(davheaders::ContentType(ct));
            *res.body_mut() = Body::from(buffer);
            return Ok(res);
        }

        // handle Depth:
        let deep = match req.headers().typed_get::<davheaders::Depth>() {
            Some(davheaders::Depth::Infinity) | None => true,
            Some(davheaders::Depth::Zero) => false,
            _ => return Err(SC::BAD_REQUEST.into()),
        };

        // handle the if-headers.
        if let Some(s) = if_match(
            req,
            meta.as_ref(),
            self.fs.as_ref(),
            &self.ls,
            &path,
            &self.credentials,
        )
        .await
        {
            return Err(s.into());
        }

        // Cut & paste from method_put.rs ....
        let mut oo = OpenOptions::write();
        oo.create = true;
        if req
            .headers()
            .typed_get::<davheaders::IfMatch>()
            .map_or(false, |h| h.0 == davheaders::ETagList::Star)
        {
            oo.create = false;
        }
        if req
            .headers()
            .typed_get::<davheaders::IfNoneMatch>()
            .map_or(false, |h| h.0 == davheaders::ETagList::Star)
        {
            oo.create_new = true;
        }

        // parse xml
        let tree = xmltree::Element::parse2(Cursor::new(xmldata))?;
        if tree.name != "lockinfo" {
            return Err(DavError::XmlParseError);
        }

        // decode Element.
        let mut shared: Option<bool> = None;
        let mut owner: Option<Element> = None;
        let mut locktype = false;

        for elem in tree.child_elems_iter() {
            match elem.name.as_str() {
                "lockscope" => {
                    let name = elem.child_elems_iter().find_map(|e| Some(e.name.as_ref()));
                    match name {
                        Some("exclusive") => shared = Some(false),
                        Some("shared") => shared = Some(true),
                        _ => return Err(DavError::XmlParseError),
                    }
                }
                "locktype" => {
                    let name = elem.child_elems_iter().find_map(|e| Some(e.name.as_ref()));
                    match name {
                        Some("write") => locktype = true,
                        _ => return Err(DavError::XmlParseError),
                    }
                }
                "owner" => {
                    let mut o = elem.clone();
                    o.prefix = Some("D".to_owned());
                    owner = Some(o);
                }
                _ => return Err(DavError::XmlParseError),
            }
        }

        // sanity check.
        if shared.is_none() || !locktype {
            return Err(DavError::XmlParseError);
        };
        let shared = shared.unwrap();

        // create lock
        let timeout = get_timeout(req, false, shared);
        let principal = self.principal.as_deref();
        let lock = match locksystem.lock(&path, principal, owner.as_ref(), timeout, shared, deep) {
            Ok(lock) => lock,
            Err(_) => return Err(SC::LOCKED.into()),
        };

        // try to create file if it doesn't exist.
        let create = oo.create;
        let create_new = oo.create_new;
        if meta.is_none() {
            match self.fs.open(&path, oo, &self.credentials).await {
                Ok(_) => {}
                Err(FsError::NotFound) | Err(FsError::Exists) => {
                    let s = if !create || create_new {
                        SC::PRECONDITION_FAILED
                    } else {
                        SC::CONFLICT
                    };
                    let _ = locksystem.unlock(&path, &lock.token);
                    return Err(s.into());
                }
                Err(e) => {
                    let _ = locksystem.unlock(&path, &lock.token);
                    return Err(e.into());
                }
            };
        }

        // output result
        let lt = format!("<{}>", lock.token);
        let ct = "application/xml; charset=utf-8".to_owned();
        res.headers_mut().typed_insert(davheaders::LockToken(lt));
        res.headers_mut().typed_insert(davheaders::ContentType(ct));
        if meta.is_none() {
            *res.status_mut() = SC::CREATED;
        } else {
            *res.status_mut() = SC::OK;
        }

        let mut emitter = xmltree_ext::emitter(MemBuffer::new())?;
        let prop = build_lock_prop(&lock, true);
        prop.write_ev(&mut emitter)?;
        let buffer = emitter.into_inner().take();

        *res.body_mut() = Body::from(buffer);
        Ok(res)
    }

    pub(crate) async fn handle_unlock(&self, req: &Request<()>) -> DavResult<Response<Body>> {
        // must have a locksystem or bail
        let locksystem = match self.ls {
            Some(ref ls) => ls,
            None => return Err(SC::METHOD_NOT_ALLOWED.into()),
        };

        // Must have Lock-Token header
        let t = req
            .headers()
            .typed_get::<davheaders::LockToken>()
            .ok_or(DavError::Status(SC::BAD_REQUEST))?;
        let token = t.0.trim_matches(|c| c == '<' || c == '>');

        let mut res = Response::new(Body::empty());

        let mut path = self.path(req);
        if let Ok(meta) = self.fs.metadata(&path, &self.credentials).await {
            self.fixpath(&mut res, &mut path, meta);
        }

        match locksystem.unlock(&path, token) {
            Ok(_) => {
                *res.status_mut() = SC::NO_CONTENT;
                Ok(res)
            }
            Err(_) => Err(SC::CONFLICT.into()),
        }
    }
}

pub(crate) fn list_lockdiscovery(ls: Option<&Box<dyn DavLockSystem>>, path: &DavPath) -> Element {
    let mut elem = Element::new2("D:lockdiscovery");

    // must have a locksystem or bail
    let locksystem = match ls {
        Some(ls) => ls,
        None => return elem,
    };

    // list the locks.
    let locks = locksystem.discover(path);
    for lock in &locks {
        elem.push_element(build_lock_prop(lock, false));
    }
    elem
}

pub(crate) fn list_supportedlock(ls: Option<&Box<dyn DavLockSystem>>) -> Element {
    let mut elem = Element::new2("D:supportedlock");

    // must have a locksystem or bail
    if ls.is_none() {
        return elem;
    }

    let mut entry = Element::new2("D:lockentry");
    let mut scope = Element::new2("D:lockscope");
    let mut ltype = Element::new2("D:locktype");
    scope.push_element(Element::new2("D:exclusive"));
    ltype.push_element(Element::new2("D:write"));
    entry.push_element(scope);
    entry.push_element(ltype);
    elem.push_element(entry);

    let mut entry = Element::new2("D:lockentry");
    let mut scope = Element::new2("D:lockscope");
    let mut ltype = Element::new2("D:locktype");
    scope.push_element(Element::new2("D:shared"));
    ltype.push_element(Element::new2("D:write"));
    entry.push_element(scope);
    entry.push_element(ltype);
    elem.push_element(entry);

    elem
}

// process timeout header
fn get_timeout(req: &Request<()>, refresh: bool, shared: bool) -> Option<Duration> {
    let max_timeout = if shared {
        Duration::new(86400, 0)
    } else {
        Duration::new(600, 0)
    };
    match req.headers().typed_get::<davheaders::Timeout>() {
        Some(davheaders::Timeout(ref vec)) if !vec.is_empty() => match vec[0] {
            DavTimeout::Infinite => {
                if refresh {
                    None
                } else {
                    Some(max_timeout)
                }
            }
            DavTimeout::Seconds(n) => Some(cmp::min(max_timeout, Duration::new(n as u64, 0))),
        },
        _ => None,
    }
}

fn build_lock_prop(lock: &DavLock, full: bool) -> Element {
    let mut actlock = Element::new2("D:activelock");

    let mut elem = Element::new2("D:lockscope");
    elem.push_element(match lock.shared {
        false => Element::new2("D:exclusive"),
        true => Element::new2("D:shared"),
    });
    actlock.push_element(elem);

    let mut elem = Element::new2("D:locktype");
    elem.push_element(Element::new2("D:write"));
    actlock.push_element(elem);

    actlock.push_element(
        Element::new2("D:depth").text(
            match lock.deep {
                false => "0",
                true => "Infinity",
            }
            .to_string(),
        ),
    );

    actlock.push_element(Element::new2("D:timeout").text(match lock.timeout {
        None => "Infinite".to_string(),
        Some(d) => format!("Second-{}", d.as_secs()),
    }));
    let mut locktokenelem = Element::new2("D:locktoken");
    locktokenelem.push_element(Element::new2("D:href").text(lock.token.clone()));
    actlock.push_element(locktokenelem);

    let mut lockroot = Element::new2("D:lockroot");
    lockroot.push_element(Element::new2("D:href").text(lock.path.with_prefix().as_url_string()));
    actlock.push_element(lockroot);

    if let Some(ref o) = lock.owner {
        actlock.push_element(o.clone());
    }

    if !full {
        return actlock;
    }

    let mut ldis = Element::new2("D:lockdiscovery");
    ldis.push_element(actlock);
    let mut prop = Element::new2("D:prop").ns("D", "DAV:");
    prop.push_element(ldis);

    prop
}
