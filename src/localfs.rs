//! Simple implementation of a DavFileSystem, basically
//! a 1:1 mapping of the std::fs interface.
//!
//! This implementation is stateless. So it is no problem, and
//! probably the easiest, to just create a new instance in your
//! handler function every time.
use std::io::ErrorKind;
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt;

use futures as futures01;
use futures03::{FutureExt,Stream,StreamExt};
use futures03::compat::{Future01CompatExt,Stream01CompatExt};

use libc;
use sha2::{self, Digest};

use crate::fs::*;
use crate::webpath::WebPath;

macro_rules! blocking {
    ($expression:expr) => ({
        let fut03 = async move {
            await!(futures01::future::poll_fn(|| {
                tokio_threadpool::blocking(|| $expression)
            }).compat())
        };
        Box::pin(fut03.then(|res| match res {
            Ok(x) => futures03::future::ready(x),
            Err(_) => panic!("the thread pool has shut down"),
        }))
    });
}

#[derive(Debug, Clone)]
pub struct LocalFs {
    basedir: PathBuf,
    public:  bool,
}

#[derive(Debug, Clone)]
struct LocalFsMetaData(std::fs::Metadata);

#[derive(Debug)]
struct LocalFsFile(std::fs::File);

#[derive(Debug)]
struct LocalFsReadDir {
    iterator: std::fs::ReadDir,
}

#[derive(Debug)]
struct LocalFsDirEntry {
    entry: std::fs::DirEntry,
}

impl LocalFs {
    /// Create a new LocalFs DavFileSystem, serving "base". If "public" is
    /// set to true, all files and directories created will be
    /// publically readable (mode 644/755), otherwise they will
    /// be private (mode 600/700). Umask stil overrides this.
    pub fn new<P: AsRef<Path>>(base: P, public: bool) -> Box<LocalFs> {
        Box::new(LocalFs {
            basedir: base.as_ref().to_path_buf(),
            public:  public,
        })
    }

    fn fspath(&self, path: &WebPath) -> PathBuf {
        path.as_pathbuf_with_prefix(&self.basedir)
    }
}

impl DavFileSystem for LocalFs {
    fn metadata<'a>(&'a self, path: &'a WebPath) -> FsFuture<Box<DavMetaData>> {
        blocking!({
            match std::fs::metadata(self.fspath(path)) {
                Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<DavMetaData>),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn symlink_metadata<'a>(&'a self, path: &'a WebPath) -> FsFuture<Box<DavMetaData>> {
        blocking!({
            match std::fs::symlink_metadata(self.fspath(path)) {
                Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<DavMetaData>),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn read_dir<'a>(&'a self, path: &'a WebPath) -> FsFuture<Pin<Box<Stream<Item=Box<DavDirEntry>> + Send>>> {
        debug!("FS: read_dir {:?}", self.fspath(path));
        blocking!({
            match std::fs::read_dir(self.fspath(path)) {
                Ok(iterator) => {
                    let stream01 = LocalFsReadDir{ iterator: iterator };
                    let stream03 = stream01
                        .compat()
                        .take_while(|res| futures03::future::ready(res.is_ok()))
                        .map(|res| res.unwrap());
                    Ok(Box::pin(stream03) as Pin<Box<Stream<Item=Box<DavDirEntry>> + Send>>)
                },
                Err(e) => Err(e.into()),
            }
        })
    }

    fn open<'a>(&'a self, path: &'a WebPath, options: OpenOptions) -> FsFuture<Box<DavFile>> {
        debug!("FS: open {:?}", self.fspath(path));
        blocking!({
            let res = std::fs::OpenOptions::new()
                .read(options.read)
                .write(options.write)
                .append(options.append)
                .truncate(options.truncate)
                .create(options.create)
                .create_new(options.create_new)
                .mode(if self.public { 0o644 } else { 0o600 })
                .open(self.fspath(path));
            match res {
                Ok(file) => Ok(Box::new(LocalFsFile(file)) as Box<DavFile>),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn create_dir<'a>(&'a self, path: &'a WebPath) -> FsFuture<()> {
        debug!("FS: create_dir {:?}", self.fspath(path));
        blocking!({
            std::fs::DirBuilder::new()
                .mode(if self.public { 0o755 } else { 0o700 })
                .create(self.fspath(path))
                .map_err(|e| e.into())
        })
    }

    fn remove_dir<'a>(&'a self, path: &'a WebPath) -> FsFuture<()> {
        debug!("FS: remove_dir {:?}", self.fspath(path));
        blocking!({
            std::fs::remove_dir(self.fspath(path)).map_err(|e| e.into())
        })
    }

    fn remove_file<'a>(&'a self, path: &'a WebPath) -> FsFuture<()> {
        debug!("FS: remove_file {:?}", self.fspath(path));
        blocking!({
            std::fs::remove_file(self.fspath(path)).map_err(|e| e.into())
        })
    }

    fn rename<'a>(&'a self, from: &'a WebPath, to: &'a WebPath) -> FsFuture<()> {
        debug!("FS: rename {:?} {:?}", self.fspath(from), self.fspath(to));
        blocking!({
            std::fs::rename(self.fspath(from), self.fspath(to)).map_err(|e| e.into())
        })
    }

    fn copy<'a>(&'a self, from: &'a WebPath, to: &'a WebPath) -> FsFuture<()> {
        debug!("FS: copy {:?} {:?}", self.fspath(from), self.fspath(to));
        blocking!({
            if let Err(e) = std::fs::copy(self.fspath(from), self.fspath(to)) {
                debug!("copy failed: {:?}", e);
                return Err(e.into());
            }
            Ok(())
        })
    }
}

/*
impl Stream for LocalFsReadDir {
    type Item = Box<DavDirEntry>;

    fn poll_next(self: Pin<&mut Self>, waker: &Waker) -> futures03::task::Poll<Option<Self::Item>> {
        let fut = futures01::future::poll_fn(|| {
            let b = tokio_threadpool::blocking(|| {
                match self.iterator.next() {
                    Some(Err(e)) => Err(e),
                    Some(Ok(entry)) => Ok(Some(Box::new(LocalFsDirEntry { entry: entry }) as Self::Item)),
                    None => Ok(None)
                }
            });
            match b {
                Ok(futures01::Async::Ready(Ok(item))) => Ok(futures::Async::Ready(item)),
                Ok(futures01::Async::NotReady) => Ok(futures::Async::NotReady),
                Ok(futures01::Async::Ready(Err(item))) => Err(item),
                Err(_) => panic!("the thread pool has shut down"),
            }
        }).compat();
        // XXX the below doesn't work. Why?
        fut.poll(waker)
    }
}
*/

impl futures01::Stream for LocalFsReadDir {
    type Item = Box<DavDirEntry>;
    type Error = FsError;

    fn poll(&mut self) -> futures01::Poll<Option<Self::Item>, Self::Error> {
        let b = tokio_threadpool::blocking(|| {
            match self.iterator.next() {
                Some(Err(e)) => Err(e),
                Some(Ok(entry)) => Ok(Some(Box::new(LocalFsDirEntry { entry: entry }) as Self::Item)),
                None => Ok(None)
            }
        });
        match b {
            Ok(futures01::Async::Ready(Ok(item))) => Ok(futures01::Async::Ready(item)),
            Ok(futures01::Async::NotReady) => Ok(futures01::Async::NotReady),
            Ok(futures01::Async::Ready(Err(e))) => Err(e.into()),
            Err(_) => panic!("the thread pool has shut down"),
        }
    }
}

impl DavDirEntry for LocalFsDirEntry {
    fn metadata<'a>(&'a self) -> FsFuture<Box<DavMetaData>> {
        blocking!({
            match self.entry.metadata() {
                Ok(meta) => Ok(Box::new(LocalFsMetaData(meta)) as Box<DavMetaData>),
                Err(e) => Err(e.into()),
            }
        })
    }

    fn name(&self) -> Vec<u8> {
        self.entry.file_name().as_bytes().to_vec()
    }

    fn is_dir<'a>(&'a self) -> FsFuture<bool> {
        blocking!({
            Ok(self.entry.file_type()?.is_dir())
        }
    }
    fn is_file<'a>(&'a self) -> FsFuture<bool> {
        blocking!({
            Ok(self.entry.file_type()?.is_file())
        }
    }
    fn is_symlink<'a>(&'a self) -> FsFuture<bool> {
        blocking!({
            Ok(self.entry.file_type()?.is_symlink())
        }
    }
}

impl DavFile for LocalFsFile {
    fn metadata<'a>(&'a self) -> FsFuture<Box<DavMetaData>> {
        blocking!({
            let meta = self.0.metadata()?;
            Ok(Box::new(LocalFsMetaData(meta)) as Box<DavMetaData>)
        })
    }

    fn write_bytes<'a>(&'a mut self, buf: &'a [u8]) -> FsFuture<usize> {
        blocking!({
            let n = self.0.write(buf)?;
            Ok(n)
        })
    }

    fn write_all<'a>(&'a mut self, buf: &'a [u8]) -> FsFuture<()> {
        blocking!({
            let len = buf.len();
            let mut pos = 0;
            while pos < len {
                let n = self.0.write(&buf[pos..])?;
                pos += n;
            }
            Ok(())
        })
    }

    fn read_bytes<'a>(&'a mut self, mut buf: &'a mut [u8]) -> FsFuture<usize> {
        blocking!({
            let n = self.0.read(&mut buf)?;
            Ok(n as usize)
        })
    }

    fn seek<'a>(&'a mut self, pos: SeekFrom) -> FsFuture<u64> {
        blocking!({
            Ok(self.0.seek(pos)?)
        })
    }

    fn flush<'a>(&'a mut self) -> FsFuture<()> {
        (blocking!({
            Ok(self.0.flush()?)
        })
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

    // #[cfg(target_os = "linux")]
    fn status_changed(&self) -> FsResult<SystemTime> {
        Ok(UNIX_EPOCH + Duration::new(self.0.st_ctime() as u64, 0))
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

    #[cfg(target_os = "linux")]
    fn etag(&self) -> String {
        fn u64_to_bytes(n: u64) -> [u8; 8] {
            unsafe { std::mem::transmute(n) }
        }
        let mut d = sha2::Sha256::default();

        // hash in modification time, filesize, inode.
        if let Ok(t) = self.0.modified() {
            if let Ok(t) = t.duration_since(UNIX_EPOCH) {
                d.input(&u64_to_bytes(t.as_secs() as u64));
                d.input(&u64_to_bytes(t.subsec_nanos() as u64));
            }
        }
        d.input(&u64_to_bytes(self.0.len()));
        d.input(&u64_to_bytes(self.0.st_ino()));
        let res = d.result();
        format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            res[0], res[1], res[2], res[3], res[4], res[5], res[6], res[7], res[8], res[9]
        )
    }
}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {
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
