
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use {Method,DavResult};
use {statuserror,daverror,fserror,fserror_to_status};
use errors::DavError;
use multierror::MultiError;
use conditional::*;
use webpath::WebPath;
use headers::{self,Depth};
use fs::*;

// map_err helper.
fn add_status(res: &mut MultiError, path: &WebPath, e: FsError) -> DavError {
    let status = fserror_to_status(e);
    if let Err(x) = res.add_status(path, status) {
        return x;
    }
    DavError::Status(status)
}

impl super::DavHandler {

    pub(crate) fn do_copy(&self, source: &WebPath, topdest: &WebPath, dest: &WebPath, depth: Depth, multierror: &mut MultiError) -> FsResult<()> {
        debug!("do_copy {} {} depth {:?}", source, dest, depth);

        // when doing "COPY /a/b /a/b/c make sure we don't recursively
        // copy /a/b/c/ into /a/b/c.
        if source == topdest {
            return Ok(())
        }

        // source must exist.
        let meta = match self.fs.metadata(source) {
            Err(e) => {
                multierror.add_status(source, fserror_to_status(e.clone())).is_ok();
                return Err(e);
            },
            Ok(m) => m,
        };

        // if it's a file we can overwrite it.
        if !meta.is_dir() {
            return match self.fs.copy(source, dest) {
                Ok(_) => Ok(()),
                Err(e) => {
                    debug!("do_copy: self.fs.copy error: {:?}", e);
                    multierror.add_status(dest, fserror_to_status(e)).is_ok();
                    Err(e)
                }
            };
        }

        // Copying a directory onto an existing directory with Depth 0
        // is not an error. It means "only copy properties" (which
        // we do not do yet).
        if let Err(e) = self.fs.create_dir(dest) {
            if depth != Depth::Zero || e != FsError::Exists {
                debug!("do_copy: self.fs.create_dir error: {:?}", e);
                multierror.add_status(dest, fserror_to_status(e)).is_ok();
                return Err(e);
            }
        }

        // only recurse when Depth > 0.
        if depth == Depth::Zero {
            return Ok(());
        }

        let entries = match self.fs.read_dir(source) {
            Ok(entries) => entries,
            Err(e) => {
                debug!("do_copy: self.fs.read_dir error: {:?}", e);
                multierror.add_status(source, fserror_to_status(e)).is_ok();
                return Err(e);
            }
        };

        // If we encounter errors, just print them, and keep going.
        // Last seen error is returned from function.
        let mut retval = Ok(());
        for dirent in entries {
            // NOTE: dirent.metadata() behaves like symlink_metadata()
            let meta = match dirent.metadata() {
                Ok(meta) => meta,
                Err(e) => {
                    multierror.add_status(source, fserror_to_status(e)).is_ok();
                    return Err(e);
                }
            };
            let mut name = dirent.name();
            let mut nsrc = source.clone();
            let mut ndest = dest.clone();
            nsrc.push_segment(&name);
            ndest.push_segment(&name);

            if meta.is_dir() {
                nsrc.add_slash();
                ndest.add_slash();
            }
            if let Err(e) = self.do_copy(&nsrc, topdest, &ndest, depth, multierror) {
                retval = Err(e);
            }
        }

        retval
    }

    pub(crate) fn do_move(&self, source: &WebPath, dest: &WebPath, existed: bool, mut multierror: MultiError) -> DavResult<()> {
        debug!("do_move {} {}", source, dest);
        if let Err(e) = self.fs.rename(source, dest) {
            // This is a single action ATM, so do not need multi-error.
            add_status(&mut multierror, source, e);
            Err(DavError::Status(multierror.close()?))
        } else {
            let s = if existed { SC::NoContent } else { SC::Created };
            multierror.finalstatus(source, s)
        }
    }

    pub(crate) fn handle_copymove(&self, method: Method, req: Request, mut res: Response) -> DavResult<()> {

        // get and check headers.
        let overwrite = req.headers.get::<headers::Overwrite>().map_or(true, |o| o.0);
        let depth = match req.headers.get::<Depth>() {
            Some(&Depth::Infinity) | None => Depth::Infinity,
            Some(&Depth::Zero) if method == Method::Copy => Depth::Zero,
            _ => return Err(statuserror(&mut res, SC::BadRequest)),
        };

        // decode and validate destination.
        let dest = req.headers.get::<headers::Destination>()
                    .ok_or(statuserror(&mut res, SC::BadRequest))?;
        let dest = match WebPath::from_str(&dest.0, &self.prefix) {
            Err(e) => Err(daverror(&mut res, e)),
            Ok(d) => Ok(d),
        }?;

        // for MOVE, tread with care- if the path ends in "/" but it actually
        // is a symlink, we want to move the symlink, not what it points to.
        let mut path = self.path(&req);
        let meta = if method == Method::Move {
            let meta = self.fs.symlink_metadata(&path).map_err(|e| fserror(&mut res, e))?;
            if meta.is_symlink() {
                let m2 = self.fs.metadata(&path).map_err(|e| fserror(&mut res, e))?;
                path.add_slash_if(m2.is_dir());
            }
            meta
        } else {
            self.fs.metadata(&path).map_err(|e| fserror(&mut res, e))?
        };
        path.add_slash_if(meta.is_dir());

        // parent of the destination must exist.
        if !self.has_parent(&dest) {
            Err(statuserror(&mut res, SC::Conflict))?;
        }

        // for the destination, also check if it's a symlink. If we are going
        // to remove it first, we want to remove the link, not what it points to.
        let (dest_is_file, dmeta) = match self.fs.symlink_metadata(&dest) {
            Ok(meta) => {
                let mut is_file = false;
                if meta.is_symlink() {
                    if let Ok(m) = self.fs.metadata(&dest) {
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
            Err(statuserror(&mut res, SC::PreconditionFailed))?;
        }

        // check if source == dest
        if path == dest {
            Err(statuserror(&mut res, SC::Forbidden))?;
        }

        // check If and If-* headers for source URL
        let tokens = match if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path) {
            Ok(t) => t,
            Err(s) => return Err(statuserror(&mut res, s)),
        };

        let mut multierror = MultiError::new(res, &path);

        // check locks. since we cancel the entire operation if there is
        // a conflicting lock, we do not return a 207 multistatus, but
        // just a simple status.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            if method == Method::Move {
                // for MOVE check if source path is locked
                if let Err(_l) = locksystem.check(&path, true, t.clone()) {
                    return multierror.finalstatus(&path, SC::Locked);
                }
            }
            // for MOVE and COPY check if destination is locked
            if let Err(_l) = locksystem.check(&dest, true, t) {
                return multierror.finalstatus(&path, SC::Locked);
            }
        }

        // see if we need to delete the destination first.
        if overwrite && exists && depth != Depth::Zero && !dest_is_file {
            debug!("handle_copymove: deleting destination {}", dest);
            if let Err(_) = self.delete_items(&mut multierror, Depth::Infinity, dmeta.unwrap(), &dest) {
                return Err(DavError::Status(multierror.close()?));
            }
            // should really do this per item, in case the delete partially fails. See TODO.md
            if let Some(ref locksystem) = self.ls {
                locksystem.delete(&dest).ok();
            }
        }

        // COPY or MOVE.
        if method == Method::Copy {
            match self.do_copy(&path, &dest, &dest, depth, &mut multierror) {
                Err(_) => return Err(DavError::Status(multierror.close()?)),
                Ok(_) => {
                    let s = if exists { SC::NoContent } else { SC::Created };
                    multierror.finalstatus(&path, s)
                }
            }
        } else {
            // move and if successful, remove locks at old location.
            self.do_move(&path, &dest, exists, multierror)?;
            if let Some(ref locksystem) = self.ls {
                locksystem.delete(&path).ok();
            }
            Ok(())
        }
    }
}

