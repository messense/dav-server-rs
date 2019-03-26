use std::path::{PathBuf,Path};
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::io::ErrorKind;

// Do a case-insensitive path lookup.
pub(crate) fn resolve<'a>(base: impl Into<PathBuf>, path: &[u8], case_insensitive: bool) -> PathBuf {

    let base = base.into();
    let mut path = Path::new(OsStr::from_bytes(path));

    // make "path" relative.
    while path.starts_with("/") {
        path = match path.strip_prefix("/") {
            Ok(p) => p,
            Err(_) => break,
        };
    }

    // if not case-mangling, return now.
    if !case_insensitive {
        let mut newpath = base;
        newpath.push(&path);
        return newpath;
    }

    // must be rooted, and valid UTF-8.
    let mut fullpath = base.clone();
    fullpath.push(&path);
    if !fullpath.has_root() || fullpath.to_str().is_none() {
        return fullpath;
    }

    // must have a parent.
    let parent = match fullpath.parent() {
        Some(p) => p,
        None => return fullpath,
    };

    // if the file exists, fine.
    if fullpath.metadata().is_ok() {
        return fullpath;
    }

    // we need the path as a list of segments.
    let segs = path.iter().collect::<Vec<_>>();
    if segs.len() == 0 {
        return fullpath;
    }

    // if the parent exists, do a lookup there straight away
    // instead of starting from the root.
    if segs.len() == 1 || parent.exists() {
        let (newpath, _) = lookup(parent.to_path_buf(), segs[segs.len() - 1], true);
        return newpath;
    }

    // start from the root, then add segments one by one.
    let mut stop = false;
    let mut newpath = base;
    for seg in segs.into_iter() {
        if !stop {
            let (n, s) = lookup(newpath, seg, false);
            newpath = n;
            stop = s;
        } else {
            newpath.push(seg);
        }
    }
    newpath
}

// lookup a filename in a directory in a case insensitive way.
fn lookup(mut path: PathBuf, seg: &OsStr, no_init_check: bool) -> (PathBuf, bool) {

    // does it exist as-is?
    let mut path2 = path.clone();
    path2.push(seg);
    if !no_init_check {
        match path2.metadata() {
            Ok(_) => return (path2, false),
            Err(ref e) if e.kind() != ErrorKind::NotFound => {
                // stop on errors other than "NotFound".
                return (path2, true)
            },
            Err(_) => {},
        }
    }

    // first, lowercase filename.
    let filename = match seg.to_str() {
        Some(s) => s.to_lowercase(),
        None => return (path2, true),
    };

    // we have to read the entire directory.
    let dir = match path.read_dir() {
        Ok(dir) => dir,
        Err(_) => return (path2, true),
    };
    for entry in dir.into_iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let entry_name = entry.file_name();
        let name = match entry_name.to_str() {
            Some(n) => n,
            None => continue,
        };
        if name.to_lowercase() == filename {
            path.push(&name);
            return (path, false);
        }
    }
    (path2, true)
}

