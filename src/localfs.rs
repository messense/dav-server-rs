//! Local filesystem access.
//!
//! This implementation is stateless. So the easiest way to use it
//! is to create a new instance in your handler every time
//! you need one.

use std::any::Any;
use std::collections::VecDeque;
use std::future::Future;
use std::io::{self, ErrorKind, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::{Buf, Bytes, BytesMut};
use futures_util::{future, future::BoxFuture, FutureExt, Stream};
use pin_utils::pin_mut;
use tokio::task;

use libc;

use crate::davpath::DavPath;
use crate::fs::*;
use crate::localfs_macos::DUCacheBuilder;

const RUNTIME_TYPE_BASIC: u32 = 1;
const RUNTIME_TYPE_THREADPOOL: u32 = 2;
static RUNTIME_TYPE: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
#[repr(u32)]
enum RuntimeType {
    Basic      = RUNTIME_TYPE_BASIC,
    ThreadPool = RUNTIME_TYPE_THREADPOOL,
}

impl RuntimeType {
    #[inline]
    fn get() -> RuntimeType {
        match RUNTIME_TYPE.load(Ordering::Relaxed) {
            RUNTIME_TYPE_BASIC => RuntimeType::Basic,
            RUNTIME_TYPE_THREADPOOL => RuntimeType::ThreadPool,
            _ => {
                let dbg = format!("{:?}", tokio::runtime::Handle::current());
                let rt = if dbg.contains("ThreadPool") {
                    RuntimeType::ThreadPool
                } else {
                    RuntimeType::Basic
                };
                RUNTIME_TYPE.store(rt as u32, Ordering::SeqCst);
                rt
            },
        }
    }
}

// Run some code via block_in_place() or spawn_blocking().
//
// There's also a method on LocalFs for this, use the freestanding
// function if you do not want the fs_access_guard() closure to be used.
#[inline]
async fn blocking<F, R>(func: F) -> R
where
    F: FnOnce() -> R,
    F: Send + 'static,
    R: Send + 'static,
{
    match RuntimeType::get() {
        RuntimeType::Basic => task::spawn_blocking(func).await.unwrap(),
        RuntimeType::ThreadPool => task::block_in_place(func),
    }
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
    pub is_file:          bool,
    pub fs_access_guard:  Option<Box<dyn Fn() -> Box<dyn Any> + Send + Sync + 'static>>,
}

#[derive(Debug)]
struct LocalFsFile(Option<std::fs::File>);

struct LocalFsReadDir {
    fs:        LocalFs,
    do_meta:   ReadDirMeta,
    buffer:    VecDeque<io::Result<LocalFsDirEntry>>,
    dir_cache: Option<DUCacheBuilder>,
    iterator:  Option<std::fs::ReadDir>,
    fut:       Option<BoxFuture<'static, ReadDirBatch>>,
}

// a DirEntry either already has the metadata available, or a handle
// to the filesystem so it can call fs.blocking()
enum Meta {
    Data(io::Result<std::fs::Metadata>),
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
            is_file:          false,
            fs_access_guard:  None,
        };
        Box::new({
            LocalFs {
                inner: Arc::new(inner),
            }
        })
    }

    /// Create a new LocalFs DavFileSystem, serving "file".
    ///
    /// This is like `new()`, but it always serves this single file.
    /// The request path is ignored.
    pub fn new_file<P: AsRef<Path>>(file: P, public: bool) -> Box<LocalFs> {
        let inner = LocalFsInner {
            basedir:          file.as_ref().to_path_buf(),
            public:           public,
            macos:            false,
            case_insensitive: false,
            is_file:          true,
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
            is_file:          false,
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
        if !self.inner.is_file {
            pathbuf.push(path.as_rel_ospath());
        }
        pathbuf
    }

    fn fspath(&self, path: &DavPath) -> PathBuf {
        if self.inner.case_insensitive {
            crate::localfs_windows::resolve(&self.inner.basedir, &path)
        } else {
            let mut pathbuf = self.inner.basedir.clone();
            if !self.inner.is_file {
                pathbuf.push(path.as_rel_ospath());
            }
            pathbuf
        }
    }

    // threadpool::blocking() adapter, also runs the before/after hooks.
    #[doc(hidden)]
    pub async fn blocking<F, R>(&self, func: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let this = self.clone();
        blocking(move || {
            let _guard = this.inner.fs_access_guard.as_ref().map(|f| f());
            func()
        })
        .await
    }
}

// This implementation is basically a bunch of boilerplate to
// wrap the std::fs call in self.blocking() calls.
impl DavFileSystem for LocalFs {
    fn metadata<'a>(&'a self, davpath: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        async move {
            if let Some(meta) = self.is_virtual(davpath) {
                return Ok(meta);
            }
            let path = self.fspath(davpath);
            if self.is_notfound(&path) {
                return Err(FsError::NotFound);
            }
            self.blocking(move || {
                match std::fs::metadata(path) {
                    Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>),
                    Err(e) => Err(e.into()),
                }
            })
            .await
        }
        .boxed()
    }

    fn symlink_metadata<'a>(&'a self, davpath: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        async move {
            if let Some(meta) = self.is_virtual(davpath) {
                return Ok(meta);
            }
            let path = self.fspath(davpath);
            if self.is_notfound(&path) {
                return Err(FsError::NotFound);
            }
            self.blocking(move || {
                match std::fs::symlink_metadata(path) {
                    Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<dyn DavMetaData>),
                    Err(e) => Err(e.into()),
                }
            })
            .await
        }
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
        async move {
            trace!("FS: read_dir {:?}", self.fspath_dbg(davpath));
            let path = self.fspath(davpath);
            let path2 = path.clone();
            let iter = self.blocking(move || std::fs::read_dir(&path)).await;
            match iter {
                Ok(iterator) => {
                    let strm = LocalFsReadDir {
                        fs:        self.clone(),
                        do_meta:   meta,
                        buffer:    VecDeque::new(),
                        dir_cache: self.dir_cache_builder(path2),
                        iterator:  Some(iterator),
                        fut:       None,
                    };
                    Ok(Box::pin(strm) as FsStream<Box<dyn DavDirEntry>>)
                },
                Err(e) => Err(e.into()),
            }
        }
        .boxed()
    }

    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions) -> FsFuture<Box<dyn DavFile>> {
        async move {
            trace!("FS: open {:?}", self.fspath_dbg(path));
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            let mode = if self.inner.public { 0o644 } else { 0o600 };
            let path = self.fspath(path);
            self.blocking(move || {
                let res = std::fs::OpenOptions::new()
                    .read(options.read)
                    .write(options.write)
                    .append(options.append)
                    .truncate(options.truncate)
                    .create(options.create)
                    .create_new(options.create_new)
                    .mode(mode)
                    .open(path);
                match res {
                    Ok(file) => Ok(Box::new(LocalFsFile(Some(file))) as Box<dyn DavFile>),
                    Err(e) => Err(e.into()),
                }
            })
            .await
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: create_dir {:?}", self.fspath_dbg(path));
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            let mode = if self.inner.public { 0o755 } else { 0o700 };
            let path = self.fspath(path);
            self.blocking(move || {
                std::fs::DirBuilder::new()
                    .mode(mode)
                    .create(path)
                    .map_err(|e| e.into())
            })
            .await
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: remove_dir {:?}", self.fspath_dbg(path));
            let path = self.fspath(path);
            self.blocking(move || std::fs::remove_dir(path).map_err(|e| e.into()))
                .await
        }
        .boxed()
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: remove_file {:?}", self.fspath_dbg(path));
            if self.is_forbidden(path) {
                return Err(FsError::Forbidden);
            }
            let path = self.fspath(path);
            self.blocking(move || std::fs::remove_file(path).map_err(|e| e.into()))
                .await
        }
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: rename {:?} {:?}", self.fspath_dbg(from), self.fspath_dbg(to));
            if self.is_forbidden(from) || self.is_forbidden(to) {
                return Err(FsError::Forbidden);
            }
            let frompath = self.fspath(from);
            let topath = self.fspath(to);
            self.blocking(move || {
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
            .await
        }
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: copy {:?} {:?}", self.fspath_dbg(from), self.fspath_dbg(to));
            if self.is_forbidden(from) || self.is_forbidden(to) {
                return Err(FsError::Forbidden);
            }
            let path_from = self.fspath(from);
            let path_to = self.fspath(to);

            match self.blocking(move || std::fs::copy(path_from, path_to)).await {
                Ok(_) => Ok(()),
                Err(e) => {
                    debug!(
                        "copy({:?}, {:?}) failed: {}",
                        self.fspath_dbg(from),
                        self.fspath_dbg(to),
                        e
                    );
                    Err(e.into())
                },
            }
        }
        .boxed()
    }
}

// read_batch() result.
struct ReadDirBatch {
    iterator: Option<std::fs::ReadDir>,
    buffer:   VecDeque<io::Result<LocalFsDirEntry>>,
}

// Read the next batch of LocalFsDirEntry structs (up to 256).
// This is sync code, must be run in `blocking()`.
fn read_batch(iterator: Option<std::fs::ReadDir>, fs: LocalFs, do_meta: ReadDirMeta) -> ReadDirBatch {
    let mut buffer = VecDeque::new();
    let mut iterator = match iterator {
        Some(i) => i,
        None => {
            return ReadDirBatch {
                buffer,
                iterator: None,
            }
        },
    };
    let _guard = match do_meta {
        ReadDirMeta::None => None,
        _ => fs.inner.fs_access_guard.as_ref().map(|f| f()),
    };
    for _ in 0..256 {
        match iterator.next() {
            Some(Ok(entry)) => {
                let meta = match do_meta {
                    ReadDirMeta::Data => Meta::Data(std::fs::metadata(entry.path())),
                    ReadDirMeta::DataSymlink => Meta::Data(entry.metadata()),
                    ReadDirMeta::None => Meta::Fs(fs.clone()),
                };
                let d = LocalFsDirEntry {
                    meta:  meta,
                    entry: entry,
                };
                buffer.push_back(Ok(d))
            },
            Some(Err(e)) => {
                buffer.push_back(Err(e));
                break;
            },
            None => break,
        }
    }
    ReadDirBatch {
        buffer,
        iterator: Some(iterator),
    }
}

impl LocalFsReadDir {
    // Create a future that calls read_batch().
    //
    // The 'iterator' is moved into the future, and returned when it completes,
    // together with a list of directory entries.
    fn read_batch(&mut self) -> BoxFuture<'static, ReadDirBatch> {
        let iterator = self.iterator.take();
        let fs = self.fs.clone();
        let do_meta = self.do_meta;

        let fut: BoxFuture<ReadDirBatch> = blocking(move || read_batch(iterator, fs, do_meta)).boxed();
        fut
    }
}

// The stream implementation tries to be smart and batch I/O operations
impl<'a> Stream for LocalFsReadDir {
    type Item = Box<dyn DavDirEntry>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = Pin::into_inner(self);

        // If the buffer is empty, fill it.
        if this.buffer.len() == 0 {
            // If we have no pending future, create one.
            if this.fut.is_none() {
                if this.iterator.is_none() {
                    return Poll::Ready(None);
                }
                this.fut = Some(this.read_batch());
            }

            // Poll the future.
            let fut = this.fut.as_mut().unwrap();
            pin_mut!(fut);
            match Pin::new(&mut fut).poll(cx) {
                Poll::Ready(batch) => {
                    this.fut.take();
                    if let Some(ref mut nb) = this.dir_cache {
                        for e in &batch.buffer {
                            if let Ok(ref e) = e {
                                nb.add(e.entry.file_name());
                            }
                        }
                    }
                    this.buffer = batch.buffer;
                    this.iterator = batch.iterator;
                },
                Poll::Pending => return Poll::Pending,
            }
        }

        // we filled the buffer, now pop from the buffer.
        match this.buffer.pop_front() {
            Some(Ok(item)) => Poll::Ready(Some(Box::new(item))),
            Some(Err(_)) | None => {
                // fuse the iterator.
                this.iterator.take();
                // finish the cache.
                if let Some(ref mut nb) = this.dir_cache {
                    nb.finish();
                }
                // return end-of-stream.
                Poll::Ready(None)
            },
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
                let fullpath = self.entry.path();
                let ft = fs
                    .blocking(move || std::fs::metadata(&fullpath))
                    .await?
                    .file_type();
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
                let fullpath = self.entry.path();
                fs.blocking(move || {
                    match std::fs::metadata(&fullpath) {
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
    fn metadata<'a>(&'a mut self) -> FsFuture<Box<dyn DavMetaData>> {
        async move {
            let file = self.0.take().unwrap();
            let (meta, file) = blocking(move || (file.metadata(), file)).await;
            self.0 = Some(file);
            Ok(Box::new(LocalFsMetaData(meta?)) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn write_bytes<'a>(&'a mut self, buf: Bytes) -> FsFuture<()> {
        async move {
            let mut file = self.0.take().unwrap();
            let (res, file) = blocking(move || (file.write_all(&buf), file)).await;
            self.0 = Some(file);
            res.map_err(|e| e.into())
        }
        .boxed()
    }

    fn write_buf<'a>(&'a mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<()> {
        async move {
            let mut file = self.0.take().unwrap();
            let (res, file) = blocking(move || {
                while buf.remaining() > 0 {
                    let n = match file.write(buf.chunk()) {
                        Ok(n) => n,
                        Err(e) => return (Err(e), file),
                    };
                    buf.advance(n);
                }
                (Ok(()), file)
            })
            .await;
            self.0 = Some(file);
            res.map_err(|e| e.into())
        }
        .boxed()
    }

    fn read_bytes<'a>(&'a mut self, count: usize) -> FsFuture<Bytes> {
        async move {
            let mut file = self.0.take().unwrap();
            let (res, file) = blocking(move || {
                let mut buf = BytesMut::with_capacity(count);
                let res = unsafe {
                    buf.set_len(count);
                    file.read(&mut buf).map(|n| {
                        buf.set_len(n);
                        buf.freeze()
                    })
                };
                (res, file)
            })
            .await;
            self.0 = Some(file);
            res.map_err(|e| e.into())
        }
        .boxed()
    }

    fn seek<'a>(&'a mut self, pos: SeekFrom) -> FsFuture<u64> {
        async move {
            let mut file = self.0.take().unwrap();
            let (res, file) = blocking(move || (file.seek(pos), file)).await;
            self.0 = Some(file);
            res.map_err(|e| e.into())
        }
        .boxed()
    }

    fn flush<'a>(&'a mut self) -> FsFuture<()> {
        async move {
            let mut file = self.0.take().unwrap();
            let (res, file) = blocking(move || (file.flush(), file)).await;
            self.0 = Some(file);
            res.map_err(|e| e.into())
        }
        .boxed()
    }
}

impl DavMetaData for LocalFsMetaData {
    fn len(&self) -> u64 {
        self.0.len()
    }
    fn created(&self) -> FsResult<SystemTime> {
        self.0.created().map_err(|e| e.into())
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

impl From<&io::Error> for FsError {
    fn from(e: &io::Error) -> Self {
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

impl From<io::Error> for FsError {
    fn from(e: io::Error) -> Self {
        (&e).into()
    }
}
