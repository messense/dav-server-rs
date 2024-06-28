use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::body::Body;
use crate::conditional::*;
use crate::davheaders;
use crate::fs::*;
use crate::{DavError, DavInner, DavResult};

impl<C: Clone + Send + Sync + 'static> DavInner<C> {
    pub(crate) async fn handle_mkcol(&self, req: &Request<()>) -> DavResult<Response<Body>> {
        let mut path = self.path(req);
        let meta = self.fs.metadata(&path, &self.credentials).await;

        // check the If and If-* headers.
        let res = if_match_get_tokens(
            req,
            meta.as_ref().ok(),
            self.fs.as_ref(),
            &self.ls,
            &path,
            &self.credentials,
        )
        .await;
        let tokens = match res {
            Ok(t) => t,
            Err(s) => return Err(DavError::Status(s)),
        };

        // if locked check if we hold that lock.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_deref();
            if let Err(_l) = locksystem.check(&path, principal, false, false, t) {
                return Err(DavError::Status(StatusCode::LOCKED));
            }
        }

        let mut res = Response::new(Body::empty());

        match self.fs.create_dir(&path, &self.credentials).await {
            // RFC 4918 9.3.1 MKCOL Status Codes.
            Err(FsError::Exists) => return Err(DavError::Status(StatusCode::METHOD_NOT_ALLOWED)),
            Err(FsError::NotFound) => return Err(DavError::Status(StatusCode::CONFLICT)),
            Err(e) => return Err(DavError::FsError(e)),
            Ok(()) => {
                if path.is_collection() {
                    path.add_slash();
                    res.headers_mut().typed_insert(davheaders::ContentLocation(
                        path.with_prefix().as_url_string(),
                    ));
                }
                *res.status_mut() = StatusCode::CREATED;
            }
        }

        Ok(res)
    }
}
