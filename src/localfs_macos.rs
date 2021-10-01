// Optimizations for macOS and the macOS finder.
//
// - after it reads a directory, macOS likes to do a PROPSTAT of all
//   files in the directory with "._" prefixed. so after each PROPSTAT
//   with Depth: 1 we keep a cache of "._" files we've seen, so that
//   we can easily tell which ones did _not_ exist.
// - deny existence of ".localized" files
// - fake a ".metadata_never_index" in the root
// - fake a ".ql_disablethumbnails" file in the root.
//
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lru::LruCache;
use parking_lot::Mutex;

use crate::davpath::DavPath;
use crate::fs::*;
use crate::localfs::LocalFs;

const DU_CACHE_ENTRIES: usize = 4096;
const DU_CACHE_MAX_AGE: u64 = 60;
const DU_CACHE_SLEEP_MS: u64 = 10037;

lazy_static! {
    static ref DU_CACHE: Arc<DUCache> = Arc::new(DUCache::new(DU_CACHE_ENTRIES));
}

static DIR_ID: AtomicUsize = AtomicUsize::new(1);

// Dot underscore cache entry.
struct Entry {
    // Time the entry in the cache was created.
    time:        SystemTime,
    // Modification time of the parent directory.
    dir_modtime: SystemTime,
    // Unique ID of the parent entry.
    dir_id:      usize,
}

// Dot underscore cache.
struct DUCache {
    cache: Mutex<LruCache<PathBuf, Entry>>,
}

impl DUCache {
    // return a new instance.
    fn new(size: usize) -> DUCache {
        thread::spawn(move || {
            loop {
                // House keeping. Every 10 seconds, remove entries older than
                // DU_CACHE_MAX_AGE seconds from the LRU cache.
                thread::sleep(Duration::from_millis(DU_CACHE_SLEEP_MS));
                {
                    let mut cache = DU_CACHE.cache.lock();
                    let now = SystemTime::now();
                    while let Some((_k, e)) = cache.peek_lru() {
                        if let Ok(age) = now.duration_since(e.time) {
                            trace!(target: "webdav_cache", "DUCache: purge check {:?}", _k);
                            if age.as_secs() <= DU_CACHE_MAX_AGE {
                                break;
                            }
                            if let Some((_k, _)) = cache.pop_lru() {
                                trace!(target: "webdav_cache", "DUCache: purging {:?} (age {})", _k, age.as_secs());
                            } else {
                                break;
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        });
        DUCache {
            cache: Mutex::new(LruCache::new(size)),
        }
    }

    // Lookup a "._filename" entry in the cache. If we are sure the path
    // does _not_ exist, return `true`.
    //
    // Note that it's assumed the file_name() DOES start with "._".
    fn negative(&self, path: &PathBuf) -> bool {
        // parent directory must be present in the cache.
        let mut dir = match path.parent() {
            Some(d) => d.to_path_buf(),
            None => return false,
        };
        dir.push(".");
        let (dir_id, dir_modtime) = {
            let cache = self.cache.lock();
            match cache.peek(&dir) {
                Some(t) => (t.dir_id, t.dir_modtime),
                None => {
                    trace!(target: "webdav_cache", "DUCache::negative({:?}): parent not in cache", path);
                    return false;
                },
            }
        };

        // Get the metadata of the parent to see if it changed.
        // This is pretty cheap, since it's most likely in the kernel cache.
        let valid = match std::fs::metadata(&dir) {
            Ok(m) => m.modified().map(|m| m == dir_modtime).unwrap_or(false),
            Err(_) => false,
        };
        let mut cache = self.cache.lock();
        if !valid {
            trace!(target: "webdav_cache", "DUCache::negative({:?}): parent in cache but stale", path);
            cache.pop(&dir);
            return false;
        }

        // Now if there is _no_ entry in the cache for this file,
        // or it is not valid (different timestamp), it did not exist
        // the last time we did a readdir().
        match cache.peek(path) {
            Some(t) => {
                trace!(target: "webdav_cache", "DUCache::negative({:?}): in cache, valid: {}", path, t.dir_id != dir_id);
                t.dir_id != dir_id
            },
            None => {
                trace!(target: "webdav_cache", "DUCache::negative({:?}): not in cache", path);
                true
            },
        }
    }
}

// Storage for the entries of one dir while we're collecting them.
#[derive(Default)]
pub(crate) struct DUCacheBuilder {
    dir:     PathBuf,
    entries: Vec<OsString>,
    done:    bool,
}

impl DUCacheBuilder {
    // return a new instance.
    pub fn start(dir: PathBuf) -> DUCacheBuilder {
        DUCacheBuilder {
            dir:     dir,
            entries: Vec::new(),
            done:    false,
        }
    }

    // add a filename to the list we have
    pub fn add(&mut self, filename: OsString) {
        if let Some(f) = Path::new(&filename).file_name() {
            if f.as_bytes().starts_with(b"._") {
                self.entries.push(filename);
            }
        }
    }

    // Process the "._" files we collected.
    //
    // We add all the "._" files we saw in the directory, and the
    // directory itself (with "/." added).
    pub fn finish(&mut self) {
        if self.done {
            return;
        }
        self.done = true;

        // Get parent directory modification time.
        let meta = match std::fs::metadata(&self.dir) {
            Ok(m) => m,
            Err(_) => return,
        };
        let dir_modtime = match meta.modified() {
            Ok(t) => t,
            Err(_) => return,
        };
        let dir_id = DIR_ID.fetch_add(1, Ordering::SeqCst);

        let now = SystemTime::now();
        let mut cache = DU_CACHE.cache.lock();

        // Add "/." to directory and store it.
        let mut path = self.dir.clone();
        path.push(".");
        let entry = Entry {
            time:        now,
            dir_modtime: dir_modtime,
            dir_id:      dir_id,
        };
        cache.put(path, entry);

        // Now add the "._" files.
        for filename in self.entries.drain(..) {
            // create full path and add it to the cache.
            let mut path = self.dir.clone();
            path.push(filename);
            let entry = Entry {
                time:        now,
                dir_modtime: dir_modtime,
                dir_id:      dir_id,
            };
            cache.put(path, entry);
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
    pub(crate) fn is_virtual(&self, path: &DavPath) -> Option<Box<dyn DavMetaData>> {
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
    pub(crate) fn is_forbidden(&self, path: &DavPath) -> bool {
        if !self.inner.macos {
            return false;
        }
        match path.as_bytes() {
            b"/.metadata_never_index" => return true,
            b"/.ql_disablethumbnails" => return true,
            _ => {},
        }
        path.file_name_bytes() == b".localized"
    }

    // File might not exists because of negative cache entry.
    #[inline]
    pub(crate) fn is_notfound(&self, path: &PathBuf) -> bool {
        if !self.inner.macos {
            return false;
        }
        match path.file_name().map(|p| p.as_bytes()) {
            Some(b".localized") => true,
            Some(name) if name.starts_with(b"._") => DU_CACHE.negative(path),
            _ => false,
        }
    }

    // Return a "directory cache builder".
    #[inline]
    pub(crate) fn dir_cache_builder(&self, path: PathBuf) -> Option<DUCacheBuilder> {
        if self.inner.macos {
            Some(DUCacheBuilder::start(path))
        } else {
            None
        }
    }
}
