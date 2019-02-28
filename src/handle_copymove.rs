use http::{Request, Response, StatusCode};

use crate::common::*;
use crate::conditional::*;
use crate::errors::*;
use crate::fs::*;
use crate::headers::{self, Depth};
use crate::makestream;
use crate::multierror::{multi_error, MultiError};
use crate::typed_headers::HeaderMapExt;
use crate::webpath::WebPath;
use crate::{BoxedByteStream, DavResult, Method};

// map_err helper.
async fn add_status<'a>(
    m_err: &'a mut MultiError,
    path: &'a WebPath,
    e: impl Into<DavError> + 'static,
) -> DavResult<()>
{
    let daverror = e.into();
    if let Err(x) = await!(m_err.add_status(path, daverror.statuscode())) {
        return Err(x.into());
    }
    Err(daverror)
}

impl crate::DavInner {
    pub(crate) fn do_copy<'a>(
        &'a self,
        source: &'a WebPath,
        topdest: &'a WebPath,
        dest: &'a WebPath,
        depth: Depth,
        mut multierror: &'a mut MultiError,
    ) -> impl Future03<Output = DavResult<()>> + Send + 'a
    {
        async move {
            // when doing "COPY /a/b /a/b/c make sure we don't recursively
            // copy /a/b/c/ into /a/b/c.
            if source == topdest {
                return Ok(());
            }

            // source must exist.
            let meta = match await!(self.fs.metadata(source)) {
                Err(e) => return await!(add_status(&mut multierror, source, e)),
                Ok(m) => m,
            };

            // if it's a file we can overwrite it.
            if !meta.is_dir() {
                return match await!(self.fs.copy(source, dest)) {
                    Ok(_) => Ok(()),
                    Err(e) => {
                        debug!("do_copy: self.fs.copy error: {:?}", e);
                        await!(add_status(&mut multierror, source, e))
                    },
                };
            }

            // Copying a directory onto an existing directory with Depth 0
            // is not an error. It means "only copy properties" (which
            // we do not do yet).
            if let Err(e) = await!(self.fs.create_dir(dest)) {
                if depth != Depth::Zero || e != FsError::Exists {
                    debug!("do_copy: self.fs.create_dir({}) error: {:?}", dest, e);
                    return await!(add_status(&mut multierror, dest, e));
                }
            }

            // only recurse when Depth > 0.
            if depth == Depth::Zero {
                return Ok(());
            }

            let mut entries = match await!(self.fs.read_dir(source, ReadDirMeta::DataSymlink)) {
                Ok(entries) => entries,
                Err(e) => {
                    debug!("do_copy: self.fs.read_dir error: {:?}", e);
                    return await!(add_status(&mut multierror, source, e));
                },
            };

            // If we encounter errors, just print them, and keep going.
            // Last seen error is returned from function.
            let mut retval = Ok::<_, DavError>(());
            while let Some(dirent) = await!(entries.next()) {
                // NOTE: dirent.metadata() behaves like symlink_metadata()
                let meta = match await!(dirent.metadata()) {
                    Ok(meta) => meta,
                    Err(e) => return await!(add_status(&mut multierror, source, e)),
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
                let recurse =
                    FutureObj03::new(Box::new(self.do_copy(&nsrc, topdest, &ndest, depth, multierror)));
                if let Err(e) = await!(recurse) {
                    retval = Err(e);
                }
            }

            retval
        }
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
        source: &'a WebPath,
        dest: &'a WebPath,
        mut multierror: &'a mut MultiError,
    ) -> DavResult<()>
    {
        if let Err(e) = await!(self.fs.rename(source, dest)) {
            await!(add_status(&mut multierror, &source, e))
        } else {
            Ok(())
        }
    }

    pub(crate) async fn handle_copymove(
        self,
        req: Request<()>,
        method: Method,
    ) -> DavResult<Response<BoxedByteStream>>
    {
        // get and check headers.
        let overwrite = req
            .headers()
            .typed_get::<headers::Overwrite>()
            .map_or(true, |o| o.0);
        let depth = match req.headers().typed_get::<Depth>() {
            Some(Depth::Infinity) | None => Depth::Infinity,
            Some(Depth::Zero) if method == Method::Copy => Depth::Zero,
            _ => return Err(StatusCode::BAD_REQUEST.into()),
        };

        // decode and validate destination.
        let dest = match req.headers().typed_get::<headers::Destination>() {
            Some(dest) => WebPath::from_str(&dest.0, &self.prefix)?,
            None => return Err(StatusCode::BAD_REQUEST.into()),
        };

        // for MOVE, tread with care- if the path ends in "/" but it actually
        // is a symlink, we want to move the symlink, not what it points to.
        let mut path = self.path(&req);
        let meta = if method == Method::Move {
            let meta = await!(self.fs.symlink_metadata(&path))?;
            if meta.is_symlink() {
                let m2 = await!(self.fs.metadata(&path))?;
                path.add_slash_if(m2.is_dir());
            }
            meta
        } else {
            await!(self.fs.metadata(&path))?
        };
        path.add_slash_if(meta.is_dir());

        // parent of the destination must exist.
        if !await!(self.has_parent(&dest)) {
            return Err(StatusCode::CONFLICT.into());
        }

        // for the destination, also check if it's a symlink. If we are going
        // to remove it first, we want to remove the link, not what it points to.
        let (dest_is_file, dmeta) = match await!(self.fs.symlink_metadata(&dest)) {
            Ok(meta) => {
                let mut is_file = false;
                if meta.is_symlink() {
                    if let Ok(m) = await!(self.fs.metadata(&dest)) {
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
        let tokens = match await!(if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path)) {
            Ok(t) => t,
            Err(s) => return Err(s.into()),
        };

        // check locks. since we cancel the entire operation if there is
        // a conflicting lock, we do not return a 207 multistatus, but
        // just a simple status.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if method == Method::Move {
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

        let items = makestream::stream03(async move |tx| {
            let mut multierror = MultiError::new(tx);

            // see if we need to delete the destination first.
            if overwrite && exists && depth != Depth::Zero && !dest_is_file {
                debug!("handle_copymove: deleting destination {}", dest);
                if let Err(_) =
                    await!(self.delete_items(&mut multierror, Depth::Infinity, dmeta.unwrap(), &dest))
                {
                    return Ok(());
                }
                // should really do this per item, in case the delete partially fails. See TODO.md
                if let Some(ref locksystem) = self.ls {
                    let _ = locksystem.delete(&dest);
                }
            }

            // COPY or MOVE.
            if method == Method::Copy {
                if let Ok(_) = await!(self.do_copy(&path, &dest, &dest, depth, &mut multierror)) {
                    let s = if exists {
                        StatusCode::NO_CONTENT
                    } else {
                        StatusCode::CREATED
                    };
                    let _ = await!(multierror.add_status(&path, s));
                }
            } else {
                // move and if successful, remove locks at old location.
                if let Ok(_) = await!(self.do_move(&path, &dest, &mut multierror)) {
                    if let Some(ref locksystem) = self.ls {
                        locksystem.delete(&path).ok();
                    }
                    let s = if exists {
                        StatusCode::NO_CONTENT
                    } else {
                        StatusCode::CREATED
                    };
                    let _ = await!(multierror.add_status(&path, s));
                }
            }
            Ok::<_, DavError>(())
        });

        await!(multi_error(req_path, items))
    }
}
