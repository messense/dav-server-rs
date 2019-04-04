// Optimizations for macOS and the macOS finder.
//
// - after each readdir, update a negative cache of "._" resourcefork
//   entries which we know do _not_ exist.
// - deny existence of ".localized" files
// - fake a ".metadata_never_index" in the root
// - fake a ".ql_disablethumbnails" file in the root.
//
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lru::LruCache;
use parking_lot::Mutex;

use crate::fs::*;
use crate::localfs::LocalFs;
use crate::webpath::WebPath;

lazy_static! {
    static ref NEG_CACHE: Arc<NegCache> = Arc::new(NegCache::new(4096));
}

// Negative cache entry.
struct NegEntry {
    // Time the entry in the cache was created.
    time: SystemTime,
    // Modification time of the parent directory.
    // If that changed the entry is invalid.
    parent_modtime: SystemTime,
}

// Negative cache.
struct NegCache {
    cache: Mutex<LruCache<PathBuf, NegEntry>>,
}

impl NegCache {
    // return a new instance.
    fn new(size: usize) -> NegCache {
        NegCache {
            cache: Mutex::new(LruCache::new(size)),
        }
    }

    // Lookup an entry in the cache, and validate it.
    // If it's invalid remove it from the cache and return false.
    fn check(&self, path: &PathBuf) -> bool {
        // Lookup.
        let mut cache = self.cache.lock();
        let e = match cache.get(path) {
            Some(t) => t,
            None => return false,
        };

        // See if it's expired. If so delete entry and return false.
        let expired = match e.time.elapsed() {
            Ok(d) => d.as_secs() > 3,
            Err(_) => true,
        };
        if expired {
            cache.pop(path);
            return false;
        }

        // Get the metadata of the parent to see if it changed.
        // This is pretty cheap, since it's most likely in the kernel cache.
        // unwrap() is safe; if the path has no parent it would not
        // be present in the cache.
        let valid = match std::fs::metadata(path.parent().unwrap()) {
            Ok(m) => m.modified().map(|m| m == e.parent_modtime).unwrap_or(false),
            Err(_) => false,
        };
        if !valid {
            cache.pop(path);
            false
        } else {
            true
        }
    }
}

// Storage for the entries of one dir while we're collecting them.
#[derive(Default)]
pub(crate) struct NegCacheBuilder {
    dir:     PathBuf,
    entries: HashSet<OsString>,
}

impl NegCacheBuilder {
    // return a new instance.
    pub fn start(dir: PathBuf) -> NegCacheBuilder {
        NegCacheBuilder {
            dir:     dir,
            entries: HashSet::new(),
        }
    }

    // add a filename to the list we have
    pub fn add(&mut self, filename: OsString) {
        self.entries.insert(filename);
    }

    // Process what we have collected.
    //
    // Prefix each entry with "._". If that name does not exist yet in
    // the directory, add it to the global negative cache list.
    pub fn finish(&mut self) {
        // Get parent directory modification time.
        let meta = match std::fs::metadata(&self.dir) {
            Ok(m) => m,
            Err(_) => return,
        };
        let parent_modtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => return,
        };

        // Process all entries.
        let now = SystemTime::now();
        let mut cache = NEG_CACHE.cache.lock();
        let mut namebuf = Vec::new();

        for e in self.entries.iter() {
            // skip if file starts with "._"
            let f = e.as_bytes();
            if f.starts_with(b"._") || f == b"." || f == b".." {
                continue;
            }

            // create "._" + filename
            namebuf.clear();
            namebuf.extend_from_slice(b"._");
            namebuf.extend_from_slice(f);
            let filename = OsStr::from_bytes(&namebuf);

            // if that exists, skip it.
            if self.entries.contains(filename) {
                continue;
            }

            // create full path and add it to the global negative cache.
            let mut path = self.dir.clone();
            path.push(filename);
            let neg = NegEntry {
                time:           now,
                parent_modtime: parent_modtime,
            };
            cache.put(path, neg);
        }
    }
}

// Fake metadata for an empty file.
#[derive(Debug, Clone)]
struct EmptyMetaData;
impl DavMetaData for EmptyMetaData {
    fn len(&self) -> u64 {
        0
    }
    fn is_dir(&self) -> bool {
        false
    }
    fn modified(&self) -> FsResult<SystemTime> {
        // Tue May 30 04:00:00 CEST 2000
        Ok(UNIX_EPOCH + Duration::new(959652000, 0))
    }
    fn created(&self) -> FsResult<SystemTime> {
        self.modified()
    }
}

impl LocalFs {
    // Is this a virtualfile ?
    #[inline]
    pub(crate) fn is_virtual(&self, path: &WebPath) -> Option<Box<DavMetaData>> {
        if !self.inner.macos {
            return None;
        }
        match path.as_bytes() {
            b"/.metadata_never_index" => {},
            b"/.ql_disablethumbnails" => {},
            _ => return None,
        }
        Some(Box::new(EmptyMetaData {}))
    }

    // This file can never exist.
    #[inline]
    pub(crate) fn is_forbidden(&self, path: &WebPath) -> bool {
        if !self.inner.macos {
            return false;
        }
        match path.as_bytes() {
            b"/.metadata_never_index" => return true,
            b"/.ql_disablethumbnails" => return true,
            _ => {},
        }
        path.file_name() == b".localized"
    }

    // File might not exists because of negative cache entry.
    #[inline]
    pub(crate) fn is_notfound(&self, path: &PathBuf) -> bool {
        if !self.inner.macos {
            return false;
        }
        match path.file_name().map(|p| p.as_bytes()) {
            Some(b".localized") => return true,
            _ => {},
        }
        NEG_CACHE.check(path)
    }

    // Return a "directory cache builder".
    #[inline]
    pub(crate) fn dir_cache_builder(&self, path: PathBuf) -> Option<NegCacheBuilder> {
        if self.inner.macos {
            Some(NegCacheBuilder::start(path))
        } else {
            None
        }
    }
}
