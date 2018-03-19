
use hyper;
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;
use headers;
use DavResult;

impl super::DavHandler {

    pub(crate) fn handle_options(&self, req: Request, mut res: Response)  -> DavResult<()> {
        {
            let h = res.headers_mut();
            if self.ls.is_some() {
                h.set(headers::DAV("1,2,3,sabredav-partialupdate".to_string()));
            } else {
                h.set(headers::DAV("1,3,sabredav-partialupdate".to_string()));
            }
            h.set(headers::MSAuthorVia("DAV".to_string()));
            h.set(hyper::header::ContentLength(0));
        }
        let meta = self.fs.metadata(&self.path(&req));
        self.do_options(&req, &mut res, meta)?;
        *res.status_mut() = SC::Ok;
        Ok(())
    }
}

