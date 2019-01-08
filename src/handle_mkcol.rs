
use http::StatusCode as SC;

use crate::sync_adapter::{Request,Response};
use crate::typed_headers::HeaderMapExt;
use crate::DavResult;
use crate::{statuserror,fserror};
use crate::conditional::*;
use crate::headers;
use crate::fs::*;

impl crate::DavInner {

    pub(crate) fn handle_mkcol(&self, req: Request, mut res: Response) -> DavResult<()> {

        let mut path = self.path(&req);
        let meta = self.fs.metadata(&path);

        // check the If and If-* headers.
        let tokens = match if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path) {
            Ok(t) => t,
            Err(s) => return Err(statuserror(&mut res, s)),
        };

        // if locked check if we hold that lock.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            if let Err(_l) = locksystem.check(&path, false, t) {
                return Err(statuserror(&mut res, SC::LOCKED));
            }
        }

        match self.fs.create_dir(&path) {
            // RFC 4918 9.3.1 MKCOL Status Codes.
            Err(FsError::Exists) => Err(statuserror(&mut res, SC::METHOD_NOT_ALLOWED)),
            Err(FsError::NotFound) => Err(statuserror(&mut res, SC::CONFLICT)),
            Err(e) => Err(fserror(&mut res, e)),
            Ok(()) => {
                if path.is_collection() {
                    path.add_slash();
                    res.headers_mut().typed_insert(headers::ContentLocation(path.as_url_string_with_prefix()));
                }
                *res.status_mut() = SC::CREATED;
                Ok(())
            }
        }
    }
}

