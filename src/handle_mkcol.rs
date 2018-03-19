
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use DavResult;
use {statuserror,fserror};
use conditional::*;
use headers;
use fs::*;

impl super::DavHandler {

    pub(crate) fn handle_mkcol(&self, req: Request, mut res: Response) -> DavResult<()> {

        let mut path = self.path(&req);
        let meta = self.fs.metadata(&path);

        // check the If and If-* headers.
        if let Some(s) = if_match(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path) {
            return Err(statuserror(&mut res, s));
        }

        match self.fs.create_dir(&path) {
            // RFC 4918 9.3.1 MKCOL Status Codes.
            Err(FsError::Exists) => Err(statuserror(&mut res, SC::MethodNotAllowed)),
            Err(FsError::NotFound) => Err(statuserror(&mut res, SC::Conflict)),
            Err(e) => Err(fserror(&mut res, e)),
            Ok(()) => {
                if path.is_collection() {
                    path.add_slash();
                    res.headers_mut().set(headers::ContentLocation(path.as_url_string_with_prefix()));
                }
                *res.status_mut() = SC::Created;
                Ok(())
            }
        }
    }
}

