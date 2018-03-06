
use std::io::Cursor;

use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use xmltree;
use xmltree::Element;
use xmltree_ext;
use xmltree_ext::ElementExt;

use uuid::Uuid;

use ls::DavLock;

use super::errors::DavError;
use super::headers::{self,Depth,Timeout};
use super::fs::{OpenOptions,FsError};
use super::{daverror,statuserror,fserror};
use super::conditional::if_match;

impl super::DavHandler {
    pub(crate) fn handle_lock(&self, mut req: Request, mut res: Response) -> Result<(), DavError> {

        // read request.
        let xmldata = self.read_request_max(&mut req, 65536);

        // must have a locksystem or bail
        let locksystem = match self.ls {
            Some(ls) => ls,
            None => return Err(statuserror(&mut res, SC::MethodNotAllowed)),
        };

        // path and meta
        let (path, meta) = match self.fixpath(&req, &mut res) {
            Ok((path, meta)) => (path, Some(meta)),
            Err(_) => (self.path(&req), None),
        };

        // process timeout header
        let timeout = match req.headers.get::<Timeout>().map(|t| t.0);

        // lock refresh?
        if xmldata.len() == 0 {

            // get locktoken
            let (_, tokens) = dav_if_match(&req, &self.fs, path);
            if tokens.len() != 1 {
                return Err(statuserror(&mut res, SC::BadRequest));
            }

            // try refresh
            let lock = match locksystem.refresh(path, &tokens[0], timeout) {
                Ok(lock) => lock,
                Err(_) => return Err(statuserror(&mut res, SC::PreConditionFailed)),
            };

            // output result
            let prop = build_lock_prop(&lock);
            *res.status_mut() = SC::Ok;
            let res = res.start()?;
            let mut emitter = xmltree_ext::emitter(res)?;
            prop.write_ev(&mut emitter)?;

            Ok(())
        }

        // handle Depth:
        let deep = match req.headers.get::<Depth>() {
            Some(&Depth::Infinity) | None => true,
            Some(&Depth::Zero)=> false,
            _ => return Err(statuserror(&mut res, SC::BadRequest)),
        };

        // handle the if-headers.
        if let Some(s) = if_match(&req, meta.as_ref(), &self.fs, &path) {
            return Err(statuserror(&mut res, s));
        }

        // Cut & paste from method_get.rs ....
        let mut oo = OpenOptions::write();
        oo.create = true;
        if req.headers.get::<headers::IfMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create_new = true;
        }
        if req.headers.get::<headers::IfNoneMatch>()
            .map_or(false, |h| &h.0 == &headers::ETagList::Star) {
                oo.create = false;
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
        let mut timeout : Option<Duration> = None;

        for mut elem in tree.children {
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
        let lock = match locksystem.lock(path, owner, timeout, shared, deep) {
            Ok(lock) => lock,
            Err(_) => return Err(statuserror(&mut res, SC::Locked)),
        };

        // try to create file if it doesn't exist.
        if let None = meta {

            match self.fs.open(&path, oo) {
                Ok(_) => {},
                Err(FsError::NotFound) |
                Err(FsError::Exists) => {
                    let s = if !oo.create || oo.create_new {
                        SC::PreconditionFailed
                    } else {
                        SC::Conflict
                    };
                    locksystem.unlock(path, &lock.token).ok();
                    return Err(statuserror(&mut res, s));
                },
                Err(e) => {
                    locksystem.unlock(path, &lock.token).ok();
                    return Err(fserror(&mut res, e));
                },
            };
        }

        // Success!
        let locktoken = Uuid::new_v4().urn().to_string();
        let timeout_at = match timeout {
            DavTimeout::Infinite => None,
            DavTimeout::Seconds(n) => Some(SystemTime::now() + n),
        };
        let lock = DavLock{
            token:      locktoken,
            path:       path,
            owner:      owner,
            timeout_at: timeout_at,
            timeout:    timeout,
            shared:     shared,
            deep:       deep,
        };

        // output result
        let prop = build_lock_prop(&lock);

        if let None = meta {
            *res.status_mut() = SC::Created;
        } else {
            *res.status_mut() = SC::Ok;
        }

        let res = res.start()?;
        let mut emitter = xmltree_ext::emitter(res)?;
        prop.write_ev(&mut emitter)?;

        Ok(())
    }

    pub(crate) fn handle_unlock(&self, req: Request, mut res: Response) -> Result<(), DavError> {
        // Must have Lock-Token header
        let t = req.headers.get::<headers::LockToken>()
            .ok_or(statuserror(&mut res, SC::BadRequest))?;
        // .. and it must look like one we handed out.
        if !t.starts_with("<urn:uuid:") {
            debug!("conflict error because of weird lock-token {}", t);
            return Err(statuserror(&mut res, SC::Conflict));
        }

        // Pretend success.
        *res.status_mut() = SC::NoContent;
        Ok(())
    }
}

pub(crate) fn list_supportedlock() -> Element {
    let mut elem = Element::new2("D:supportedlock");

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

pub(crate) fn list_lockdiscovery() -> Element {
    Element::new2("D:lockdiscovery")
}

fn build_lock_prop(lock: &DavLock) -> Element {
    let mut actlock = Element::new2("D:activelock");

    let mut elem = Element::new2("D:lockscope");
    elem.push(match lock.shared {
        false	=> Element::new2("D:exclusive"),
        true	=> Element::new2("D:shared"),
    });
    actlock.push(elem);

    let mut elem = Element::new2("D:locktype");
    elem.push(match lockinfo.ltype {
        LockType::Write => Element::new2("D:write"),
    });
    actlock.push(elem);

    actlock.push(Element::new2("D:depth").text(match lock.deep {
        false	=> "0",
        true	=> "Infinity",
    }.to_string()));

    actlock.push(Element::new2("D:timeout").text(match lock.timeout {
		DavTimeout::Infinite => "Infinite".to_string(),
		DavTimeout::Seconds(n) => format!("Second-{}", n),
	});
    let mut locktokenelem = Element::new2("D:locktoken");
    locktokenelem.push(Element::new2("D:href").text(lock.token.clone()));
    actlock.push(locktokenelem);

    let mut lockroot = Element::new2("D:lockroot");
    lockroot.push(Element::new2("D:href").text(lock.path.as_url_string_with_prefix()));
    actlock.push(lockroot);

    if let Some(o) = lock.owner {
        actlock.push(o);
    }

    let mut ldis = Element::new2("D:lockdiscovery");
    ldis.push(lock);
    let mut prop = Element::new2("D:prop").ns("D", "DAV:");
    prop.push(ldis);

	prop
}

