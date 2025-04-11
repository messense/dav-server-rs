// Optimizations for windows and the windows webdav mini-redirector.
//
// The main thing here is case-insensitive path lookups,
// and caching that.
//
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::ErrorKind;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lru::LruCache;
use parking_lot::Mutex;

use crate::davpath::DavPath;

const CACHE_ENTRIES: usize = 4096;
const CACHE_MAX_AGE: u64 = 15 * 60;
const CACHE_SLEEP_MS: u64 = 30059;

lazy_static! {
    static ref CACHE: Arc<Cache> = Arc::new(Cache::new(CACHE_ENTRIES));
}

// Do a case-insensitive path lookup.
pub(crate) fn resolve(base: impl Into<PathBuf>, path: &DavPath) -> PathBuf {
    let base = base.into();
    let path = path.as_rel_ospath();

    // must be rooted, and valid UTF-8.
    let mut fullpath = base.clone();
    fullpath.push(path);
    if !fullpath.has_root() || fullpath.to_str().is_none() {
        return fullpath;
    }

    // must have a parent.
    let parent = match fullpath.parent() {
        Some(p) => p,
        None => return fullpath,
    };

    // deref in advance: first lazy_static, then Arc.
    let cache = &*CACHE;

    // In the cache?
    if let Some((path, _)) = cache.get(&fullpath) {
        return path;
    }

    // if the file exists, fine.
    if fullpath.metadata().is_ok() {
        return fullpath;
    }

    // we need the path as a list of segments.
    let segs = path.iter().collect::<Vec<_>>();
    if segs.is_empty() {
        return fullpath;
    }

    // if the parent exists, do a lookup there straight away
    // instead of starting from the root.
    let (parent, parent_exists) = if segs.len() > 1 {
        match cache.get(parent) {
            Some((path, _)) => (path, true),
            None => {
                let exists = parent.exists();
                if exists {
                    cache.insert(parent);
                }
                (parent.to_path_buf(), exists)
            }
        }
    } else {
        (parent.to_path_buf(), true)
    };
    if parent_exists {
        let (newpath, stop) = lookup(parent, segs[segs.len() - 1], true);
        if !stop {
            cache.insert(&newpath);
        }
        return newpath;
    }

    // start from the root, then add segments one by one.
    let mut stop = false;
    let mut newpath = base;
    let lastseg = segs.len() - 1;
    for (idx, seg) in segs.into_iter().enumerate() {
        if !stop {
            if idx == lastseg {
                // Save the path leading up to this file or dir.
                cache.insert(&newpath);
            }
            let (n, s) = lookup(newpath, seg, false);
            newpath = n;
            stop = s;
        } else {
            newpath.push(seg);
        }
    }
    if !stop {
        // resolved succesfully. save in cache.
        cache.insert(&newpath);
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
                return (path2, true);
            }
            Err(_) => {}
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
            path.push(name);
            return (path, false);
        }
    }
    (path2, true)
}

// The cache stores a mapping of lowercased path -> actual path.
pub struct Cache {
    cache: Mutex<LruCache<PathBuf, Entry>>,
}

#[derive(Clone)]
struct Entry {
    // Full case-sensitive pathname.
    path: PathBuf,
    // Unix timestamp.
    time: u64,
}

// helper
fn pathbuf_to_lowercase(path: PathBuf) -> PathBuf {
    let s = match OsString::from(path).into_string() {
        Ok(s) => OsString::from(s.to_lowercase()),
        Err(s) => s,
    };
    PathBuf::from(s)
}

impl Cache {
    pub fn new(size: usize) -> Cache {
        thread::spawn(move || {
            // House keeping. Every 30 seconds, remove entries older than
            // CACHE_MAX_AGE seconds from the LRU cache.
            loop {
                thread::sleep(Duration::from_millis(CACHE_SLEEP_MS));
                if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
                    let now = d.as_secs();
                    let mut cache = CACHE.cache.lock();
                    while let Some((_k, e)) = cache.peek_lru() {
                        trace!(target: "webdav_cache", "Cache: purge check: {:?}", _k);
                        if e.time + CACHE_MAX_AGE > now {
                            break;
                        }
                        let _age = now - e.time;
                        if let Some((_k, _)) = cache.pop_lru() {
                            trace!(target: "webdav_cache", "Cache: purging {:?} (age {})", _k, _age);
                        } else {
                            break;
                        }
                    }
                    drop(cache);
                }
            }
        });
        Cache {
            cache: Mutex::new(LruCache::new(NonZeroUsize::new(size).unwrap())),
        }
    }

    // Insert an entry into the cache.
    pub fn insert(&self, path: &Path) {
        let lc_path = pathbuf_to_lowercase(PathBuf::from(path));
        if let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let e = Entry {
                path: PathBuf::from(path),
                time: d.as_secs(),
            };
            let mut cache = self.cache.lock();
            cache.put(lc_path, e);
        }
    }

    // Get an entry from the cache, and validate it. If it's valid
    // return the actual pathname and metadata. If it's invalid remove
    // it from the cache and return None.
    pub fn get(&self, path: &Path) -> Option<(PathBuf, fs::Metadata)> {
        // First lowercase the entire path.
        let lc_path = pathbuf_to_lowercase(PathBuf::from(path));
        // Lookup.
        let e = {
            let mut cache = self.cache.lock();
            cache.get(&lc_path)?.clone()
        };
        // Found, validate.
        match fs::metadata(&e.path) {
            Err(_) => {
                let mut cache = self.cache.lock();
                cache.pop(&lc_path);
                None
            }
            Ok(m) => Some((e.path, m)),
        }
    }
}
