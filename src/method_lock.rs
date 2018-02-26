
use std::io::Cursor;

use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use xmltree;
use xmltree::Element;
use xmltree_ext;
use xmltree_ext::ElementExt;

use uuid::Uuid;

use super::errors::DavError;
use super::headers::{self,Depth};
use super::fs::{OpenOptions,FsError};
use super::{daverror,statuserror,fserror};
use super::conditional::if_match;

#[derive(Debug)]
enum LockScope {
    Shared,
    Exclusive,
}

#[derive(Debug)]
enum LockType {
    Write
}

#[derive(Debug)]
struct LockInfo {
    scope: LockScope,
    ltype: LockType,
    owner: Option<Element>,
}

impl super::DavHandler {
    pub(crate) fn handle_lock(&self, mut req: Request, mut res: Response) -> Result<(), DavError> {

        // read request.
        let xmldata = self.read_request_max(&mut req, 65536);

        let depth = match req.headers.get::<Depth>() {
            Some(&Depth::Infinity) | None => Depth::Infinity,
            Some(&Depth::Zero)=> Depth::Zero,
            _ => return Err(statuserror(&mut res, SC::BadRequest)),
        };

        let (path, meta) = match self.fixpath(&req, &mut res) {
            Ok((path, meta)) => (path, Some(meta)),
            Err(_) => (self.path(&req), None),
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
        let mut locktype : Option<LockType> = None;
        let mut lockscope : Option<LockScope> = None;
        let mut owner : Option<Element> = None;

        for mut elem in tree.children {
            match elem.name.as_str() {
                "lockscope" if elem.children.len() == 1 => {
                    match elem.children[0].name.as_ref() {
                        "exclusive" => lockscope = Some(LockScope::Exclusive),
                        "shared" => lockscope = Some(LockScope::Shared),
                        _ => return Err(DavError::XmlParseError),
                    }
                },
                "locktype" if elem.children.len() == 1 => {
                    match elem.children[0].name.as_ref() {
                        "write" => locktype = Some(LockType::Write),
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

        let lockinfo = match (locktype, lockscope) {
            (Some(t), Some(s)) => LockInfo{
                scope: s,
                ltype: t,
                owner: owner,
            },
            _ => return Err(DavError::XmlParseError),
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
                    return Err(statuserror(&mut res, s))
                },
                Err(e) => return Err(fserror(&mut res, e)),
            };
        }

        // Claim we succeeded.
        let locktoken = Uuid::new_v4().urn().to_string();

        let mut lock = Element::new2("D:activelock");

        let mut elem = Element::new2("D:lockscope");
        elem.push(match lockinfo.scope {
            LockScope::Shared => Element::new2("D:shared"),
            LockScope::Exclusive => Element::new2("D:exclusive"),
        });
        lock.push(elem);

        let mut elem = Element::new2("D:locktype");
        elem.push(match lockinfo.ltype {
            LockType::Write => Element::new2("D:write"),
        });
        lock.push(elem);

        lock.push(Element::new2("D:depth").text(match depth {
            Depth::Zero => "0",
            Depth::One => "1",
            Depth::Infinity => "Infinity",
        }.to_string()));

        lock.push(Element::new2("D:timeout").text("Second-3600".to_string()));

        let mut locktokenelem = Element::new2("D:locktoken");
        locktokenelem.push(Element::new2("D:href").text(locktoken.clone()));
        lock.push(locktokenelem);

        let mut lockroot = Element::new2("D:lockroot");
        lockroot.push(Element::new2("D:href").text(path.as_url_string_with_prefix()));
        lock.push(lockroot);

        if let Some(o) = lockinfo.owner {
            lock.push(o);
        }

        let mut ldis = Element::new2("D:lockdiscovery");
        ldis.push(lock);
        let mut prop = Element::new2("D:prop").ns("D", "DAV:");
        prop.push(ldis);

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
