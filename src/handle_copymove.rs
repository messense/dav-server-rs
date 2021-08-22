use futures_util::{future::BoxFuture, FutureExt, StreamExt};
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::async_stream::AsyncStream;
use crate::body::Body;
use crate::conditional::*;
use crate::davheaders::{self, Depth};
use crate::davpath::DavPath;
use crate::errors::*;
use crate::fs::*;
use crate::multierror::{multi_error, MultiError};
use crate::{util::DavMethod, DavResult};

// map_err helper.
async fn add_status<'a>(
    m_err: &'a mut MultiError,
    path: &'a DavPath,
    e: impl Into<DavError> + 'static,
) -> DavResult<()>
{
    let daverror = e.into();
    if let Err(x) = m_err.add_status(path, daverror.statuscode()).await {
        return Err(x.into());
    }
    Err(daverror)
}

impl crate::DavInner {
    pub(crate) fn do_copy<'a>(
        &'a self,
        source: &'a DavPath,
        topdest: &'a DavPath,
        dest: &'a DavPath,
        depth: Depth,
        mut multierror: &'a mut MultiError,
    ) -> BoxFuture<'a, DavResult<()>>
    {
        async move {
            // when doing "COPY /a/b /a/b/c make sure we don't recursively
            // copy /a/b/c/ into /a/b/c.
            if source == topdest {
                return Ok(());
            }

            // source must exist.
            let meta = match self.fs.metadata(source).await {
                Err(e) => return add_status(&mut multierror, source, e).await,
                Ok(m) => m,
            };

            // if it's a file we can overwrite it.
            if !meta.is_dir() {
                return match self.fs.copy(source, dest).await {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        debug!("do_copy: self.fs.copy error: {:?}", e);
                        add_status(&mut multierror, source, e).await
                    },
                };
            }

            // Copying a directory onto an existing directory with Depth 0
            // is not an error. It means "only copy properties" (which
            // we do not do yet).
            if let Err(e) = self.fs.create_dir(dest).await {
                if depth != Depth::Zero || e != FsError::Exists {
                    debug!("do_copy: self.fs.create_dir({}) error: {:?}", dest, e);
                    return add_status(&mut multierror, dest, e).await;
                }
            }

            // only recurse when Depth > 0.
            if depth == Depth::Zero {
                return Ok(());
            }

            let mut entries = match self.fs.read_dir(source, ReadDirMeta::DataSymlink).await {
                Ok(entries) => entries,
                Err(e) => {
                    debug!("do_copy: self.fs.read_dir error: {:?}", e);
                    return add_status(&mut multierror, source, e).await;
                },
            };

            // If we encounter errors, just print them, and keep going.
            // Last seen error is returned from function.
            let mut retval = Ok::<_, DavError>(());
            while let Some(dirent) = entries.next().await {
                // NOTE: dirent.metadata() behaves like symlink_metadata()
                let meta = match dirent.metadata().await {
                    Ok(meta) => meta,
                    Err(e) => return add_status(&mut multierror, source, e).await,
                };
                let name = dirent.name();
                let mut nsrc = source.clone();
                let mut ndest = dest.clone();
                nsrc.push_segment(&name);
                ndest.push_segment(&name);

                if meta.is_dir() {
                    nsrc.add_slash();
                    ndest.add_slash();
                }
                // recurse.
                if let Err(e) = self.do_copy(&nsrc, topdest, &ndest, depth, multierror).await {
                    retval = Err(e);
                }
            }

            retval
        }
        .boxed()
    }

    // Right now we handle MOVE with a simple RENAME. RFC4918 #9.9.2 talks
    // about "partially failed moves", which means that we might have to
    // try to move directories with increasing granularity to move as much
    // as possible instead of all-or-nothing.
    //
    // Note that this might not be optional, as the RFC says:
    //
    //  "Any headers included with MOVE MUST be applied in processing every
    //   resource to be moved with the exception of the Destination header."
    //
    // .. so for perfect compliance we might have to process all resources
    // one-by-one anyway. But seriously, who cares.
    //
    pub(crate) async fn do_move<'a>(
        &'a self,
        source: &'a DavPath,
        dest: &'a DavPath,
        mut multierror: &'a mut MultiError,
    ) -> DavResult<()>
    {
        if let Err(e) = self.fs.rename(source, dest).await {
            add_status(&mut multierror, &source, e).await
        } else {
            Ok(())
        }
    }

    pub(crate) async fn handle_copymove(
        self,
        req: &Request<()>,
        method: DavMethod,
    ) -> DavResult<Response<Body>>
    {
        // get and check headers.
        let overwrite = req
            .headers()
            .typed_get::<davheaders::Overwrite>()
            .map_or(true, |o| o.0);
        let depth = match req.headers().typed_get::<Depth>() {
            Some(Depth::Infinity) | None => Depth::Infinity,
            Some(Depth::Zero) if method == DavMethod::Copy => Depth::Zero,
            _ => return Err(StatusCode::BAD_REQUEST.into()),
        };

        // decode and validate destination.
        let dest = match req.headers().typed_get::<davheaders::Destination>() {
            Some(dest) => DavPath::from_str_and_prefix(&dest.0, &self.prefix)?,
            None => return Err(StatusCode::BAD_REQUEST.into()),
        };

        // for MOVE, tread with care- if the path ends in "/" but it actually
        // is a symlink, we want to move the symlink, not what it points to.
        let mut path = self.path(&req);
        let meta = if method == DavMethod::Move {
            let meta = self.fs.symlink_metadata(&path).await?;
            if meta.is_symlink() {
                let m2 = self.fs.metadata(&path).await?;
                path.add_slash_if(m2.is_dir());
            }
            meta
        } else {
            self.fs.metadata(&path).await?
        };
        path.add_slash_if(meta.is_dir());

        // parent of the destination must exist.
        if !self.has_parent(&dest).await {
            return Err(StatusCode::CONFLICT.into());
        }

        // for the destination, also check if it's a symlink. If we are going
        // to remove it first, we want to remove the link, not what it points to.
        let (dest_is_file, dmeta) = match self.fs.symlink_metadata(&dest).await {
            Ok(meta) => {
                let mut is_file = false;
                if meta.is_symlink() {
                    if let Ok(m) = self.fs.metadata(&dest).await {
                        is_file = m.is_file();
                    }
                }
                if meta.is_file() {
                    is_file = true;
                }
                (is_file, Ok(meta))
            },
            Err(e) => (false, Err(e)),
        };

        // check if overwrite is "F"
        let exists = dmeta.is_ok();
        if !overwrite && exists {
            return Err(StatusCode::PRECONDITION_FAILED.into());
        }

        // check if source == dest
        if path == dest {
            return Err(StatusCode::FORBIDDEN.into());
        }

        // check If and If-* headers for source URL
        let tokens = match if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path).await {
            Ok(t) => t,
            Err(s) => return Err(s.into()),
        };

        // check locks. since we cancel the entire operation if there is
        // a conflicting lock, we do not return a 207 multistatus, but
        // just a simple status.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if method == DavMethod::Move {
                // for MOVE check if source path is locked
                if let Err(_l) = locksystem.check(&path, principal, false, true, t.clone()) {
                    return Err(StatusCode::LOCKED.into());
                }
            }
            // for MOVE and COPY check if destination is locked
            if let Err(_l) = locksystem.check(&dest, principal, false, true, t) {
                return Err(StatusCode::LOCKED.into());
            }
        }

        let req_path = path.clone();

        let items = AsyncStream::new(|tx| {
            async move {
                let mut multierror = MultiError::new(tx);

                // see if we need to delete the destination first.
                if overwrite && exists && depth != Depth::Zero && !dest_is_file {
                    trace!("handle_copymove: deleting destination {}", dest);
                    if let Err(_) = self
                        .delete_items(&mut multierror, Depth::Infinity, dmeta.unwrap(), &dest)
                        .await
                    {
                        return Ok(());
                    }
                    // should really do this per item, in case the delete partially fails. See TODO.md
                    if let Some(ref locksystem) = self.ls {
                        let _ = locksystem.delete(&dest);
                    }
                }

                // COPY or MOVE.
                if method == DavMethod::Copy {
                    if let Ok(_) = self.do_copy(&path, &dest, &dest, depth, &mut multierror).await {
                        let s = if exists {
                            StatusCode::NO_CONTENT
                        } else {
                            StatusCode::CREATED
                        };
                        let _ = multierror.add_status(&path, s).await;
                    }
                } else {
                    // move and if successful, remove locks at old location.
                    if let Ok(_) = self.do_move(&path, &dest, &mut multierror).await {
                        if let Some(ref locksystem) = self.ls {
                            locksystem.delete(&path).ok();
                        }
                        let s = if exists {
                            StatusCode::NO_CONTENT
                        } else {
                            StatusCode::CREATED
                        };
                        let _ = multierror.add_status(&path, s).await;
                    }
                }
                Ok::<_, DavError>(())
            }
        });

        multi_error(req_path, items).await
    }
}
