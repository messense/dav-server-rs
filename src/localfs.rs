//! Local filesystem access.
//!
//! This implementation is stateless. So the easiest way to use it
//! is to create a new instance in your handler every time
//! you need one.

use std::any::Any;
use std::collections::VecDeque;
use std::future::Future;
use std::io::ErrorKind;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::{future, FutureExt, Stream};
use pin_utils::pin_mut;
use tokio::task::block_in_place;

use libc;

use crate::davpath::DavPath;
use crate::fs::*;
use crate::localfs_macos::DUCacheBuilder;

// Run some code via tokio_executor::threadpool::blocking().
//
// There's also a method on LocalFs for this, use the freestanding
// function if you do not want the fs_access_guard() closure to be used.
async fn blocking<F, R>(func: F) -> R
where F: FnOnce() -> R {
    block_in_place(func)
}

#[derive(Debug, Clone)]
struct LocalFsMetaData(std::fs::Metadata);

/// Local Filesystem implementation.
#[derive(Clone)]
pub struct LocalFs {
    pub(crate) inner: Arc<LocalFsInner>,
}

// inner struct.
pub(crate) struct LocalFsInner {
    pub basedir:          PathBuf,
    pub public:           bool,
    pub case_insensitive: bool,
    pub macos:            bool,
    pub fs_access_guard:  Option<Box<dyn Fn() -> Box<dyn Any> + Send + Sync + 'static>>,
}

#[derive(Debug)]
struct LocalFsFile(std::fs::File);

struct LocalFsReadDir {
    fs:        LocalFs,
    do_meta:   ReadDirMeta,
    buffer:    VecDeque<std::io::Result<LocalFsDirEntry>>,
    dir_cache: Option<DUCacheBuilder>,
    iterator:  std::fs::ReadDir,
}

// a DirEntry either already has the metadata available, or a handle
// to the filesystem so it can call fs.blocking()
enum Meta {
    Data(std::io::Result<std::fs::Metadata>),
    Fs(LocalFs),
}

// Items from the readdir stream.
struct LocalFsDirEntry {
    meta:  Meta,
    entry: std::fs::DirEntry,
}

impl LocalFs {
    /// Create a new LocalFs DavFileSystem, serving "base".
    ///
    /// If "public" is set to true, all files and directories created will be
    /// publically readable (mode 644/755), otherwise they will be private
    /// (mode 600/700). Umask stil overrides this.
    ///
    /// If "case_insensitive" is set to true, all filesystem lookups will
    /// be case insensitive. Note that this has a _lot_ of overhead!
    pub fn new<P: AsRef<Path>>(base: P, public: bool, case_insensitive: bool, macos: bool) -> Box<LocalFs> {
        let inner = LocalFsInner {
            basedir:          base.as_ref().to_path_buf(),
            public:           public,
            macos:            macos,
            case_insensitive: case_insensitive,
            fs_access_guard:  None,
        };
        Box::new({
            LocalFs {
                inner: Arc::new(inner),
            }
        })
    }

    // Like new() but pass in a fs_access_guard hook.
    #[doc(hidden)]
    pub fn new_with_fs_access_guard<P: AsRef<Path>>(
        base: P,
        public: bool,
        case_insensitive: bool,
        macos: bool,
        fs_access_guard: Option<Box<dyn Fn() -> Box<dyn Any> + Send + Sync + 'static>>,
    ) -> Box<LocalFs>
    {
        let inner = LocalFsInner {
            basedir:          base.as_ref().to_path_buf(),
            public:           public,
            macos:            macos,
            case_insensitive: case_insensitive,
            fs_access_guard:  fs_access_guard,
        };
        Box::new({
            LocalFs {
                inner: Arc::new(inner),
            }
        })
    }

    fn fspath_dbg(&self, path: &DavPath) -> PathBuf {
        let mut pathbuf = self.inner.basedir.clone();
        pathbuf.push(path.as_rel_ospath());
        pathbuf
    }

    fn fspath(&self, path: &DavPath) -> PathBuf {
        if self.inner.case_insensitive {
            crate::localfs_windows::resolve(&self.inner.basedir, &path)
        } else {
            let mut pathbuf = self.inner.basedir.clone();
            pathbuf.push(path.as_rel_ospath());
            pathbuf
        }
    }

    // threadpool::blocking() adapter, also runs the before/after hooks.
    #[doc(hidden)]
    pub async fn blocking<F, R>(&self, func: F) -> R
    where F: FnOnce() -> R {
        block_in_place(move || {
            let _guard = self.inner.fs_access_guard.as_ref().map(|f| f());
            func()
        })
    }
}

// This implementation is basically a bunch of boilerplate to
// wrap the std::fs call in self.blocking() calls.
impl DavFileSystem for LocalFs {
    fn metadata<'a>(&'a self, davpath: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        self.blocking(move || {
            if let Some(meta) = self.is_virtual(davpath) {
                return Ok(meta);
            }
            let path = self.fspath(davpath);
            if self.is_notfound(&path) {
                return Err(FsError::NotFound);
            }
            match std::fs::metadata(path) {
                Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>),
                Err(e) => Err(e.into()),
            }
        })
        .boxed()
    }

    fn symlink_metadata<'a>(&'a self, davpath: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        self.blocking(move || {
            if let Some(meta) = self.is_virtual(davpath) {
                return Ok(meta);
            }
            let path = self.fspath(davpath);
            if self.is_notfound(&path) {
                return Err(FsError::NotFound);
            }
            match std::fs::symlink_metadata(path) {
                Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>),
                Err(e) => Err(e.into()),
            }
        })
        .boxed()
    }

    // read_dir is a bit more involved - but not much - than a simple wrapper,
    // because it returns a stream.
    fn read_dir<'a>(
        &'a self,
        davpath: &'a DavPath,
        meta: ReadDirMeta,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>>
    {
        debug!("FS: read_dir {:?}", self.fspath_dbg(davpath));
        self.blocking(move || {
            let path = self.fspath(davpath);
            match std::fs::read_dir(&path) {
                Ok(iterator) => {
                    let strm = LocalFsReadDir {
                        fs:        self.clone(),
                        do_meta:   meta,
                        buffer:    VecDeque::new(),
                        dir_cache: self.dir_cache_builder(path),
                        iterator:  iterator,
                    };
                    Ok(Box::pin(strm) as FsStream<Box<dyn DavDirEntry>>)
                },
                Err(e) => Err(e.into()),
            }
        })
        .boxed()
    }

    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions) -> FsFuture<Box<dyn DavFile>> {
        debug!("FS: open {:?}", self.fspath_dbg(path));
        self.blocking(move || {
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            let res = std::fs::OpenOptions::new()
                .read(options.read)
                .write(options.write)
                .append(options.append)
                .truncate(options.truncate)
                .create(options.create)
                .create_new(options.create_new)
                .mode(if self.inner.public { 0o644 } else { 0o600 })
                .open(self.fspath(path));
            match res {
                Ok(file) => Ok(Box::new(LocalFsFile(file)) as Box<dyn DavFile>),
                Err(e) => Err(e.into()),
            }
        })
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        debug!("FS: create_dir {:?}", self.fspath_dbg(path));
        self.blocking(move || {
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            std::fs::DirBuilder::new()
                .mode(if self.inner.public { 0o755 } else { 0o700 })
                .create(self.fspath(path))
                .map_err(|e| e.into())
        })
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        debug!("FS: remove_dir {:?}", self.fspath_dbg(path));
        self.blocking(move || std::fs::remove_dir(self.fspath(path)).map_err(|e| e.into()))
            .boxed()
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        debug!("FS: remove_file {:?}", self.fspath_dbg(path));
        self.blocking(move || {
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            std::fs::remove_file(self.fspath(path)).map_err(|e| e.into())
        })
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        debug!("FS: rename {:?} {:?}", self.fspath_dbg(from), self.fspath_dbg(to));
        self.blocking(move || {
            if self.is_forbidden(from) || self.is_forbidden(to) {
                return Err(FsError::Forbidden);
            }
            let frompath = self.fspath(from);
            let topath = self.fspath(to);
            match std::fs::rename(&frompath, &topath) {
                Ok(v) => Ok(v),
                Err(e) => {
                    // webdav allows a rename from a directory to a file.
                    // note that this check is racy, and I'm not quite sure what
                    // we should do if the source is a symlink. anyway ...
                    if e.raw_os_error() == Some(libc::ENOTDIR) && frompath.is_dir() {
                        // remove and try again.
                        let _ = std::fs::remove_file(&topath);
                        std::fs::rename(frompath, topath).map_err(|e| e.into())
                    } else {
                        Err(e.into())
                    }
                },
            }
        })
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        debug!("FS: copy {:?} {:?}", self.fspath_dbg(from), self.fspath_dbg(to));
        self.blocking(move || {
            if self.is_forbidden(from) || self.is_forbidden(to) {
                return Err(FsError::Forbidden);
            }
            if let Err(e) = std::fs::copy(self.fspath(from), self.fspath(to)) {
                debug!("copy failed: {:?}", e);
                return Err(e.into());
            }
            Ok(())
        })
        .boxed()
    }
}

// The stream implementation tries to be smart and batch I/O operations
impl Stream for LocalFsReadDir {
    type Item = Box<dyn DavDirEntry>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut futures::task::Context<'_>,
    ) -> futures::task::Poll<Option<Self::Item>>
    {
        use futures::task::Poll;

        // We buffer up to 256 entries, so that we batch the blocking() calls.
        if self.buffer.len() == 0 {
            let fut = blocking(|| {
                let _guard = match self.do_meta {
                    ReadDirMeta::None => None,
                    _ => self.fs.inner.fs_access_guard.as_ref().map(|f| f()),
                };
                for _ in 0..256 {
                    match self.iterator.next() {
                        Some(Ok(entry)) => {
                            let meta = match self.do_meta {
                                ReadDirMeta::Data => Meta::Data(std::fs::metadata(entry.path())),
                                ReadDirMeta::DataSymlink => Meta::Data(entry.metadata()),
                                ReadDirMeta::None => Meta::Fs(self.fs.clone()),
                            };
                            if let Some(ref mut nb) = self.dir_cache {
                                nb.add(entry.file_name());
                            }
                            let d = LocalFsDirEntry {
                                meta:  meta,
                                entry: entry,
                            };
                            self.buffer.push_back(Ok(d))
                        },
                        Some(Err(e)) => {
                            self.buffer.push_back(Err(e));
                            break;
                        },
                        None => {
                            if let Some(ref mut nb) = self.dir_cache {
                                nb.finish();
                            }
                            break;
                        },
                    }
                }
            });
            pin_mut!(fut);
            match Pin::new(&mut fut).poll(cx) {
                Poll::Ready(_) => {},
                Poll::Pending => return Poll::Pending,
            }
        }

        // we filled the buffer, now pop from the buffer.
        match self.buffer.pop_front() {
            Some(Ok(item)) => Poll::Ready(Some(Box::new(item))),
            Some(Err(_e)) => Poll::Ready(None),
            None => Poll::Ready(None),
        }
    }
}

enum Is {
    File,
    Dir,
    Symlink,
}

impl LocalFsDirEntry {
    async fn is_a(&self, is: Is) -> FsResult<bool> {
        match self.meta {
            Meta::Data(Ok(ref meta)) => {
                Ok(match is {
                    Is::File => meta.file_type().is_file(),
                    Is::Dir => meta.file_type().is_dir(),
                    Is::Symlink => meta.file_type().is_symlink(),
                })
            },
            Meta::Data(Err(ref e)) => Err(e.into()),
            Meta::Fs(ref fs) => {
                let ft = fs.blocking(move || self.entry.metadata()).await?.file_type();
                Ok(match is {
                    Is::File => ft.is_file(),
                    Is::Dir => ft.is_dir(),
                    Is::Symlink => ft.is_symlink(),
                })
            },
        }
    }
}

impl DavDirEntry for LocalFsDirEntry {
    fn metadata<'a>(&'a self) -> FsFuture<Box<dyn DavMetaData>> {
        match self.meta {
            Meta::Data(ref meta) => {
                let m = match meta {
                    Ok(meta) => Ok(Box::new(LocalFsMetaData(meta.clone())) as Box<dyn DavMetaData>),
                    Err(e) => Err(e.into()),
                };
                Box::pin(future::ready(m))
            },
            Meta::Fs(ref fs) => {
                fs.blocking(move || {
                    match self.entry.metadata() {
                        Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>),
                        Err(e) => Err(e.into()),
                    }
                })
                .boxed()
            },
        }
    }

    fn name(&self) -> Vec<u8> {
        self.entry.file_name().as_bytes().to_vec()
    }

    fn is_dir<'a>(&'a self) -> FsFuture<bool> {
        Box::pin(self.is_a(Is::Dir))
    }

    fn is_file<'a>(&'a self) -> FsFuture<bool> {
        Box::pin(self.is_a(Is::File))
    }

    fn is_symlink<'a>(&'a self) -> FsFuture<bool> {
        Box::pin(self.is_a(Is::Symlink))
    }
}

impl DavFile for LocalFsFile {
    fn metadata<'a>(&'a self) -> FsFuture<Box<dyn DavMetaData>> {
        blocking(move || {
            let meta = self.0.metadata()?;
            Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>)
        })
        .boxed()
    }

    fn write_bytes<'a>(&'a mut self, buf: &'a [u8]) -> FsFuture<usize> {
        blocking(move || {
            let n = self.0.write(buf)?;
            Ok(n)
        })
        .boxed()
    }

    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> FsFuture<()> {
        blocking(move || {
            let len = buf.len();
            let mut pos = 0;
            while pos < len {
                let n = self.0.write(&buf[pos..])?;
                pos += n;
            }
            Ok(())
        })
        .boxed()
    }

    fn read_bytes<'a>(&'a mut self, mut buf: &'a mut [u8]) -> FsFuture<usize> {
        blocking(move || {
            let n = self.0.read(&mut buf)?;
            Ok(n as usize)
        })
        .boxed()
    }

    fn seek<'a>(&'a mut self, pos: SeekFrom) -> FsFuture<u64> {
        Box::pin(future::ready(self.0.seek(pos).map_err(|e| e.into())))
    }

    fn flush<'a>(&'a mut self) -> FsFuture<()> {
        blocking(move || Ok(self.0.flush()?)).boxed()
    }
}

impl DavMetaData for LocalFsMetaData {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn modified(&self) -> FsResult<SystemTime> {
        self.0.modified().map_err(|e| e.into())
    }
    fn accessed(&self) -> FsResult<SystemTime> {
        self.0.accessed().map_err(|e| e.into())
    }

    fn status_changed(&self) -> FsResult<SystemTime> {
        Ok(UNIX_EPOCH + Duration::new(self.0.ctime() as u64, 0))
    }

    fn is_dir(&self) -> bool {
        self.0.is_dir()
    }
    fn is_file(&self) -> bool {
        self.0.is_file()
    }
    fn is_symlink(&self) -> bool {
        self.0.file_type().is_symlink()
    }
    fn executable(&self) -> FsResult<bool> {
        if self.0.is_file() {
            return Ok((self.0.permissions().mode() & 0o100) > 0);
        }
        Err(FsError::NotImplemented)
    }

    // same as the default apache etag.
    fn etag(&self) -> Option<String> {
        let modified = self.0.modified().ok()?;
        let t = modified.duration_since(UNIX_EPOCH).ok()?;
        let t = t.as_secs() * 1000000 + t.subsec_nanos() as u64 / 1000;
        if self.is_file() {
            Some(format!("{:x}-{:x}-{:x}", self.0.ino(), self.0.len(), t))
        } else {
            Some(format!("{:x}-{:x}", self.0.ino(), t))
        }
    }
}

impl From<&std::io::Error> for FsError {
    fn from(e: &std::io::Error) -> Self {
        if let Some(errno) = e.raw_os_error() {
            // specific errors.
            match errno {
                libc::EMLINK | libc::ENOSPC | libc::EDQUOT => return FsError::InsufficientStorage,
                libc::EFBIG => return FsError::TooLarge,
                libc::EACCES | libc::EPERM => return FsError::Forbidden,
                libc::ENOTEMPTY | libc::EEXIST => return FsError::Exists,
                libc::ELOOP => return FsError::LoopDetected,
                libc::ENAMETOOLONG => return FsError::PathTooLong,
                libc::ENOTDIR => return FsError::Forbidden,
                libc::EISDIR => return FsError::Forbidden,
                libc::EROFS => return FsError::Forbidden,
                libc::ENOENT => return FsError::NotFound,
                libc::ENOSYS => return FsError::NotImplemented,
                libc::EXDEV => return FsError::IsRemote,
                _ => {},
            }
        } else {
            // not an OS error - must be "not implemented"
            // (e.g. metadata().created() on systems without st_crtime)
            return FsError::NotImplemented;
        }
        // generic mappings for-whatever is left.
        match e.kind() {
            ErrorKind::NotFound => FsError::NotFound,
            ErrorKind::PermissionDenied => FsError::Forbidden,
            _ => FsError::GeneralFailure,
        }
    }
}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
        (&e).into()
    }
}
