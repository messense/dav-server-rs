
use http::StatusCode as SC;

use crate::sync_adapter::{Request,Response};
use crate::typed_headers::{HeaderMapExt};
use crate::DavResult;
use crate::{statuserror,fserror,fserror_to_status};
use crate::errors::DavError;
use crate::multierror::MultiError;
use crate::conditional::*;
use crate::webpath::WebPath;
use crate::headers::Depth;
use crate::fs::*;

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
        FsError::Exists => SC::CONFLICT,
        e => fserror_to_status(e),
    };
    if let Err(x) = res.add_status(path, status) {
        return x;
    }
    DavError::Status(status)
}

impl crate::DavInner {

    pub(crate) fn delete_items(&self, mut res: &mut MultiError, depth: Depth, meta: Box<DavMetaData>, path: &WebPath) -> DavResult<()> {
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
            // NOTE: dirent.metadata == symlink_metadata (!)
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

    pub(crate) fn handle_delete(&self, req: Request, mut res: Response) -> DavResult<()> {

        // RFC4918 9.6.1 DELETE for Collections.
        // Not that allowing Depth: 0 is NOT RFC compliant.
        let depth = match req.headers.typed_get::<Depth>() {
            Some(Depth::Infinity) | None => Depth::Infinity,
            Some(Depth::Zero) => Depth::Zero,
            _ => return Err(statuserror(&mut res, SC::BAD_REQUEST)),
        };

        let mut path = self.path(&req);
        let meta = self.fs.symlink_metadata(&path).map_err(|e| fserror(&mut res, e))?;
        if meta.is_symlink() {
            if let Ok(m2) = self.fs.metadata(&path) {
                path.add_slash_if(m2.is_dir());
            }
        }
        path.add_slash_if(meta.is_dir());

        // check the If and If-* headers.
        let tokens = match if_match_get_tokens(&req, Some(&meta), &self.fs, &self.ls, &path) {
            Ok(t) => t,
            Err(s) => return Err(statuserror(&mut res, s)),
        };

        // check locks. since we cancel the entire operation if there is
        // a conflicting lock, we do not return a 207 multistatus, but
        // just a simple status.
        if let Some(ref locksystem) = self.ls {
            let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
            let principal = self.principal.as_ref().map(|s| s.as_str());
            if let Err(_l) = locksystem.check(&path, principal, false, true, t) {
                return Err(statuserror(&mut res, SC::LOCKED));
            }
        }

        let mut multierror = MultiError::new(res, &path);

        if let Ok(()) = self.delete_items(&mut multierror, depth, meta, &path) {
            // should really do this per resource, in case the delete partially fails. See TODO.pm
            if let Some(ref locksystem) = self.ls {
                locksystem.delete(&path).ok();
            }
            return multierror.finalstatus(&path, SC::NO_CONTENT);
        }

        let status = multierror.close()?;
        Err(DavError::Status(status))
    }
}

