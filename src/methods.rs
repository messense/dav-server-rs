
use hyper;
use hyper::server::{Request,Response};
use hyper::status::StatusCode as SC;

use {Method,DavResult};
use webpath::WebPath;
use errors::DavError;
use {statuserror,daverror,fserror,fserror_to_status};
use fs::*;
use multierror::MultiError;
use headers;
use headers::Depth;

// map_err helper.
fn add_status(res: &mut MultiError, path: &WebPath, e: FsError) -> DavError {
    let status = fserror_to_status(e);
    if let Err(x) = res.add_status(path, status) {
        return x;
    }
    DavError::Status(status)
}

// map_err helper for directories, the result statuscode
// mappings are not 100% the same.
fn dir_status(res: &mut MultiError, path: &WebPath, e: FsError) -> DavError {
    let status = match e {
        FsError::Exists => SC::Conflict,
        e => fserror_to_status(e),
    };
    if let Err(x) = res.add_status(path, status) {
        return x;
    }
    DavError::Status(status)
}

impl super::DavHandler {

    // OPTIONS
    pub(crate) fn handle_options(&self, req: Request, mut res: Response)  -> DavResult<()> {
        {
            let h = res.headers_mut();
            h.set(headers::DAV("1,2,3,sabredav-partialupdate".to_string()));
            h.set(headers::MSAuthorVia("DAV".to_string()));
            h.set(hyper::header::ContentLength(0));
        }
        let meta = self.fs.metadata(&self.path(&req));
        self.do_options(&req, &mut res, meta)?;
        *res.status_mut() = SC::Ok;
        Ok(())
    }

    // MKCOL.
    pub(crate) fn handle_mkcol(&self, req: Request, mut res: Response) -> DavResult<()> {

        let mut path = self.path(&req);
        match self.fs.create_dir(&path) {
            // RFC 4918 9.3.1 MKCOL Status Codes.
            Err(FsError::Exists) => Err(statuserror(&mut res, SC::MethodNotAllowed)),
            Err(FsError::NotFound) => Err(statuserror(&mut res, SC::Conflict)),
            Err(e) => Err(fserror(&mut res, e)),
            Ok(()) => {
                if path.is_collection() {
                    path.add_slash();
                    res.headers_mut().set(headers::ContentLocation(path.as_url_string()));
                }
                *res.status_mut() = SC::Created;
                Ok(())
            }
        }
    }

    fn delete_items(&self, mut res: &mut MultiError, depth: Depth, meta: Box<DavMetaData>, path: &WebPath) -> DavResult<()> {
        if !meta.is_dir() {
            debug!("delete_items (file) {} {:?}", path, depth);
            return self.fs.remove_file(path).map_err(|e| add_status(&mut res, path, e));
        }
        if depth == Depth::Zero {
            debug!("delete_items (dir) {} {:?}", path, depth);
            return self.fs.remove_dir(path).map_err(|e| dir_status(&mut res, path, e));
        }
        debug!("delete_items (recurse) {} {:?}", path, depth);

        // walk over all entries.
        let entries = self.fs.read_dir(path).map_err(|e| add_status(&mut res, path, e))?;
        let mut result = Ok(());
        for dirent in entries {
            // if metadata() fails, skip to next entry.
            let meta = match dirent.metadata() {
                Ok(m) => m,
                Err(e) => { result = Err(add_status(&mut res, path, e)); continue },
            };

            let mut npath = path.clone();
            npath.push_segment(&dirent.name());
            npath.add_slash_if(meta.is_dir());

            // do the actual work. If this fails with a non-fs related error,
            // return immediately.
            if let Err(e) = self.delete_items(&mut res, depth, meta, &npath) {
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

        self.fs.remove_dir(path).map_err(|e| dir_status(&mut res, path, e))
    }

    // DELETE
    pub(crate) fn handle_delete(&self, req: Request, mut res: Response) -> DavResult<()> {

        // RFC4918 9.6.1 DELETE for Collections.
        // Not that allowing Depth: 0 is NOT RFC compliant.
        let depth = match req.headers.get::<Depth>() {
            Some(&Depth::Infinity) | None => Depth::Infinity,
            Some(&Depth::Zero) => Depth::Zero,
            _ => return Err(statuserror(&mut res, SC::BadRequest)),
        };

        let (path, meta) = self.fixpath(&req, &mut res).map_err(|e| fserror(&mut res, e))?;
        let mut multierror = MultiError::new(res, &path);

        if let Ok(()) = self.delete_items(&mut multierror, depth, meta, &path) {
            return multierror.finalstatus(&path, SC::NoContent);
        }

        let status = multierror.close()?;
        Err(DavError::Status(status))
    }

    // COPY
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
        if let Err(e) = self.fs.rename(source, dest) {
            // XXX FIXME probably need to check if the failure was
            // source or destionation related and produce the
            // correct error & path.
            add_status(&mut multierror, source, e);
            Err(DavError::Status(multierror.close()?))
        } else {
            let s = if existed { SC::NoContent } else { SC::Created };
            multierror.finalstatus(source, s)
        }
    }

    // COPY / MOVE
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

        // source must exist, as well as the parent of the destination.
        let path = self.path(&req);
        self.fs.metadata(&path).map_err(|e| fserror(&mut res, e))?;
        if !self.has_parent(&dest) {
            Err(statuserror(&mut res, SC::Conflict))?;
        }

        // check if overwrite is "F"
        let dmeta = self.fs.metadata(&dest);
        let exists = dmeta.is_ok();
        if !overwrite && exists {
            Err(statuserror(&mut res, SC::PreconditionFailed))?;
        }

        // check if source == dest
        if path == dest {
            Err(statuserror(&mut res, SC::Forbidden))?;
        }

        let mut multierror = MultiError::new(res, &path);

        // see if we need to delete the destination first.
        if overwrite && exists && depth != Depth::Zero {
            debug!("handle_copymove: deleting destination {}", dest);
            if let Err(_) = self.delete_items(&mut multierror, Depth::Infinity, dmeta.unwrap(), &dest) {
                return Err(DavError::Status(multierror.close()?));
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
            self.do_move(&path, &dest, exists, multierror)
        }
    }
}

