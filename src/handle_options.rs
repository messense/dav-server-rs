use headers::HeaderMapExt;
use http::{Request, Response};

use crate::body::Body;
use crate::util::{dav_method, DavMethod};
use crate::{DavInner, DavResult};

impl<C: Clone + Send + Sync + 'static> DavInner<C> {
    pub(crate) async fn handle_options(&self, req: &Request<()>) -> DavResult<Response<Body>> {
        let mut res = Response::new(Body::empty());

        let h = res.headers_mut();

        // We could simply not report webdav level 2 support if self.allow doesn't
        // contain LOCK/UNLOCK. However we do advertise support, since there might
        // be LOCK/UNLOCK support in another part of the URL space.
        let dav = "1,2,3,sabredav-partialupdate";
        h.insert("DAV", dav.parse().unwrap());
        h.insert("MS-Author-Via", "DAV".parse().unwrap());
        h.typed_insert(headers::ContentLength(0));

        // Helper to add method to array if method is in fact
        // allowed. If the current method is not OPTIONS, leave
        // out the current method since we're probably called
        // for DavMethodNotAllowed.
        let method = dav_method(req.method()).unwrap_or(DavMethod::Options);
        let islock = |m| m == DavMethod::Lock || m == DavMethod::Unlock;
        let mm = |v: &mut Vec<String>, m: &str, y: DavMethod| {
            if (y == DavMethod::Options || (y != method || islock(y) != islock(method)))
                && (!islock(y) || self.ls.is_some())
                && self.allow.map(|x| x.contains(y)).unwrap_or(true)
            {
                v.push(m.to_string());
            }
        };

        let path = self.path(req);
        let meta = self.fs.metadata(&path, &self.credentials).await;
        let is_unmapped = meta.is_err();
        let is_file = meta.map(|m| m.is_file()).unwrap_or_default();
        let is_star = path.is_star() && method == DavMethod::Options;

        let mut v = Vec::new();
        if is_unmapped && !is_star {
            mm(&mut v, "OPTIONS", DavMethod::Options);
            mm(&mut v, "MKCOL", DavMethod::MkCol);
            mm(&mut v, "PUT", DavMethod::Put);
            mm(&mut v, "LOCK", DavMethod::Lock);
        } else {
            if is_file || is_star {
                mm(&mut v, "HEAD", DavMethod::Head);
                mm(&mut v, "GET", DavMethod::Get);
                mm(&mut v, "PATCH", DavMethod::Patch);
                mm(&mut v, "PUT", DavMethod::Put);
            }
            mm(&mut v, "OPTIONS", DavMethod::Options);
            mm(&mut v, "PROPFIND", DavMethod::PropFind);
            mm(&mut v, "COPY", DavMethod::Copy);
            if path.as_url_string() != "/" {
                mm(&mut v, "MOVE", DavMethod::Move);
                mm(&mut v, "DELETE", DavMethod::Delete);
            }
            mm(&mut v, "LOCK", DavMethod::Lock);
            mm(&mut v, "UNLOCK", DavMethod::Unlock);
        }

        let a = v.join(",").parse().unwrap();
        res.headers_mut().insert("allow", a);

        Ok(res)
    }
}
