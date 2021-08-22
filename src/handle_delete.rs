use futures_util::{future::BoxFuture, FutureExt, StreamExt};
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::conditional::if_match_get_tokens;
use crate::davheaders::Depth;
use crate::davpath::DavPath;
use crate::errors::*;
use crate::fs::*;
use crate::multierror::{multi_error, MultiError};
use crate::DavResult;

// map_err helper.
async fn add_status<'a>(m_err: &'a mut MultiError, path: &'a DavPath, e: FsError) -> DavError {
    let status = DavError::FsError(e).statuscode();
    if let Err(x) = m_err.add_status(path, status).await {
        return x.into();
    }
    DavError::Status(status)
}

// map_err helper for directories, the result statuscode
// mappings are not 100% the same.
async fn dir_status<'a>(res: &'a mut MultiError, path: &'a DavPath, e: FsError) -> DavError {
    let status = match e {
        FsError::Exists => StatusCode::CONFLICT,
        e => DavError::FsError(e).statuscode(),
    };
    if let Err(x) = res.add_status(path, status).await {
        return x.into();
    }
    DavError::Status(status)
}

impl crate::DavInner {
    pub(crate) fn delete_items<'a>(
        &'a self,
        mut res: &'a mut MultiError,
        depth: Depth,
        meta: Box<dyn DavMetaData + 'a>,
        path: &'a DavPath,
    ) -> BoxFuture<'a, DavResult<()>>
    {
        async move {
            if !meta.is_dir() {
                trace!("delete_items (file) {} {:?}", path, depth);
                return match self.fs.remove_file(path).await {
                    Ok(x) => Ok(x),
                    Err(e) => Err(add_status(&mut res, path, e).await),
                };
            }
            if depth == Depth::Zero {
                trace!("delete_items (dir) {} {:?}", path, depth);
                return match self.fs.remove_dir(path).await {
                    Ok(x) => Ok(x),
                    Err(e) => Err(add_status(&mut res, path, e).await),
                };
            }

            // walk over all entries.
            let mut entries = match self.fs.read_dir(path, ReadDirMeta::DataSymlink).await {
                Ok(x) => Ok(x),
                Err(e) => Err(add_status(&mut res, path, e).await),
            }?;

            let mut result = Ok(());
            while let Some(dirent) = entries.next().await {
                // if metadata() fails, skip to next entry.
                // NOTE: dirent.metadata == symlink_metadata (!)
                let meta = match dirent.metadata().await {
                    Ok(m) => m,
                    Err(e) => {
                        result = Err(add_status(&mut res, path, e).await);
                        continue;
                    },
                };

                let mut npath = path.clone();
                npath.push_segment(&dirent.name());
                npath.add_slash_if(meta.is_dir());

                // do the actual work. If this fails with a non-fs related error,
                // return immediately.
                if let Err(e) = self.delete_items(&mut res, depth, meta, &npath).await {
                    match e {
                        DavError::Status(_) => {
                            result = Err(e);
                            continue;
                        },
                        _ => return Err(e),
                    }
                }
            }

            // if we got any error, return with the error,
            // and do not try to remove the directory.
            result?;

            match self.fs.remove_dir(path).await {
                Ok(x) => Ok(x),
                Err(e) => Err(dir_status(&mut res, path, e).await),
            }
        }
        .boxed()
    }

    pub(crate) async fn handle_delete(self, req: &Request<()>) -> DavResult<Response<Body>> {
        // RFC4918 9.6.1 DELETE for Collections.
        // Note that allowing Depth: 0 is NOT RFC compliant.
        let depth = match req.headers().typed_get::<Depth>() {
            Some(Depth::Infinity) | None => Depth::Infinity,
            Some(Depth::Zero) => Depth::Zero,
            _ => return Err(DavError::Status(StatusCode::BAD_REQUEST)),
        };

        let mut path = self.path(&req);
        let meta = self.fs.symlink_metadata(&path).await?;
        if meta.is_symlink() {
            if let Ok(m2) = self.fs.metadata(&path).await {
                path.add_slash_if(m2.is_dir());
            }
        }
        path.add_slash_if(meta.is_dir());

        // check the If and If-* headers.
        let tokens_res = if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path).await;
        let tokens = match tokens_res {
            Ok(t) => t,
            Err(s) => return Err(DavError::Status(s)),
        };

        // check locks. since we cancel the entire operation if there is
        // a conflicting lock, we do not return a 207 multistatus, but
        // just a simple status.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if let Err(_l) = locksystem.check(&path, principal, false, true, t) {
                return Err(DavError::Status(StatusCode::LOCKED));
            }
        }

        let req_path = path.clone();

        let items = AsyncStream::new(|tx| {
            async move {
                // turn the Sink into something easier to pass around.
                let mut multierror = MultiError::new(tx);

                // now delete the path recursively.
                let fut = self.delete_items(&mut multierror, depth, meta, &path);
                if let Ok(()) = fut.await {
                    // Done. Now delete the path in the locksystem as well.
                    // Should really do this per resource, in case the delete partially fails. See TODO.pm
                    if let Some(ref locksystem) = self.ls {
                        locksystem.delete(&path).ok();
                    }
                    let _ = multierror.add_status(&path, StatusCode::NO_CONTENT).await;
                }
                Ok(())
            }
        });

        multi_error(req_path, items).await
    }
}
