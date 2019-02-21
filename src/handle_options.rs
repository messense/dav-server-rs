use http::{Request, Response};

use crate::common::*;
use crate::headers;
use crate::typed_headers::{self, HeaderMapExt};
use crate::{dav_method, empty_body, BoxedByteStream, DavResult, Method};

impl crate::DavInner {
    pub(crate) fn handle_options(
        self,
        req: Request<()>,
    ) -> impl Future03<Output = DavResult<Response<BoxedByteStream>>>
    {
        async move {
            let mut res = Response::new(empty_body());

            let h = res.headers_mut();
            if self.ls.is_some() {
                h.typed_insert(headers::DAV("1,2,3,sabredav-partialupdate".to_string()));
            } else {
                h.typed_insert(headers::DAV("1,3,sabredav-partialupdate".to_string()));
            }
            h.typed_insert(headers::MSAuthorVia("DAV".to_string()));
            h.typed_insert(typed_headers::ContentLength(0));

            // Helper to add method to array if method is in fact
            // allowed. If the current method is not OPTIONS, leave
            // out the current method since we're probably called
            // for MethodNotAllowed.
            let method = dav_method(req.method()).unwrap_or(Method::Options);
            let islock = |m| m == Method::Lock || m == Method::Unlock;
            let mm = |v: &mut Vec<String>, m: &str, y: Method| {
                if (y == Method::Options || (y != method || islock(y) != islock(method))) &&
                    (!islock(y) || self.ls.is_some()) &&
                    self.allow.map(|x| x.allowed(y)).unwrap_or(true)
                {
                    v.push(m.to_string());
                }
            };

            let path = self.path(&req);
            let meta = blocking_io!(self.fs.metadata(&path));
            let is_unmapped = meta.is_err();
            let is_file = meta.and_then(|m| Ok(m.is_file())).unwrap_or_default();
            let is_star = path.is_star() && method == Method::Options;

            let mut v = Vec::new();
            if is_unmapped && !is_star {
                mm(&mut v, "OPTIONS", Method::Options);
                mm(&mut v, "MKCOL", Method::MkCol);
                mm(&mut v, "PUT", Method::Put);
                mm(&mut v, "LOCK", Method::Lock);
            } else {
                if is_file || is_star {
                    mm(&mut v, "HEAD", Method::Head);
                    mm(&mut v, "GET", Method::Get);
                    mm(&mut v, "PATCH", Method::Patch);
                    mm(&mut v, "PUT", Method::Put);
                }
                mm(&mut v, "OPTIONS", Method::Options);
                mm(&mut v, "PROPFIND", Method::PropFind);
                mm(&mut v, "COPY", Method::Copy);
                if path.as_url_string() != "/" {
                    mm(&mut v, "MOVE", Method::Move);
                    mm(&mut v, "DELETE", Method::Delete);
                }
                mm(&mut v, "LOCK", Method::Lock);
                mm(&mut v, "UNLOCK", Method::Unlock);
            }

            let a = v.clone().join(",").parse().unwrap();
            res.headers_mut().insert("allow", a);

            Ok(res)
        }
    }
}
