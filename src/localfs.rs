//!  Simple implementation of a DavFileSystem, basically
//!  a 1:1 mapping of the std::fs interface.
//!
use std;
use std::io::{Read,Write,Seek,SeekFrom};
use std::io::Result as IoResult;
use std::path::{Path,PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::OpenOptionsExt;
use std::time::{Duration,UNIX_EPOCH,SystemTime};
use std::io::ErrorKind;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::DirBuilderExt;

use libc;

#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt;

use sha2::{self,Digest};

use webpath::WebPath;
use fs::*;

#[derive(Debug,Clone)]
pub struct LocalFs {
    basedir:    PathBuf,
    public:     bool,
}

#[derive(Debug)]
struct LocalFsMetaData(std::fs::Metadata);

#[derive(Debug)]
struct LocalFsFile(std::fs::File);

#[derive(Debug)]
struct LocalFsReadDir {
    path:       WebPath,
    iterator:   std::fs::ReadDir,
}

#[derive(Debug)]
struct LocalFsDirEntry {
    entry:      std::fs::DirEntry,
    name:       Vec<u8>,
}

impl LocalFs {
    /// Create a new LocalFs DavFileSystem, serving "base". If "public" is
    /// set to true, all files and directories created will be
    /// publically readable (mode 644/755), otherwise they will
    /// be private (mode 600/700). Umask stil overrides this.
    pub fn new<P: AsRef<Path>>(base: P, public: bool) -> Box<LocalFs> {
        Box::new(LocalFs{
            basedir: base.as_ref().to_path_buf(),
            public: public,
        })
    }

    fn fspath(&self, path: &WebPath) -> PathBuf {
        path.as_pathbuf_with_prefix(&self.basedir)
    }
}

impl DavFileSystem for LocalFs {

    // boilerplate helper so that clone() works.
    fn box_clone(&self) -> Box<DavFileSystem> {
        Box::new((*self).clone())
    }

    fn metadata(&self, path: &WebPath) -> FsResult<Box<DavMetaData>> {
        match std::fs::metadata(self.fspath(path)) {
            Ok(meta) => Ok(Box::new(LocalFsMetaData(meta))),
            Err(e) => Err(e.into())
        }
    }

    fn symlink_metadata(&self, path: &WebPath) -> FsResult<Box<DavMetaData>> {
        match std::fs::symlink_metadata(self.fspath(path)) {
            Ok(meta) => Ok(Box::new(LocalFsMetaData(meta))),
            Err(e) => Err(e.into())
        }
    }

    fn read_dir(&self, path: &WebPath) -> FsResult<Box<DavReadDir<Item=Box<DavDirEntry>>>> {
        debug!("FS: read_dir {:?}", self.fspath(path));
        match std::fs::read_dir(self.fspath(path)) {
            Ok(iterator) => Ok(Box::new(LocalFsReadDir{
                path:       path.to_owned(),
                iterator:   iterator,
            })),
            Err(e) => Err(e.into())
        }
    }

    fn open(&self, path: &WebPath, options: OpenOptions) -> FsResult<Box<DavFile>> {
        debug!("FS: open {:?}", self.fspath(path));
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
            Ok(file) => Ok(Box::new(LocalFsFile(file))),
            Err(e) => Err(e.into())
        }
    }

    fn create_dir(&self, path: &WebPath) -> FsResult<()> {
        debug!("FS: create_dir {:?}", self.fspath(path));
        std::fs::DirBuilder::new()
            .mode(if self.public { 0o755 } else { 0o700 })
            .create(self.fspath(path)).map_err(|e| e.into())
    }

    fn remove_dir(&self, path: &WebPath) -> FsResult<()> {
        debug!("FS: remove_dir {:?}", self.fspath(path));
        std::fs::remove_dir(self.fspath(path)).map_err(|e| e.into())
    }

    fn remove_file(&self, path: &WebPath) -> FsResult<()> {
        debug!("FS: remove_file {:?}", self.fspath(path));
        std::fs::remove_file(self.fspath(path)).map_err(|e| e.into())
    }

    fn rename(&self, from: &WebPath, to: &WebPath) -> FsResult<()> {
        debug!("FS: rename {:?} {:?}", self.fspath(from), self.fspath(to));
        std::fs::rename(self.fspath(from), self.fspath(to)).map_err(|e| e.into())
    }

    fn copy(&self, from: &WebPath, to: &WebPath) -> FsResult<()> {
        debug!("FS: copy {:?} {:?}", self.fspath(from), self.fspath(to));
        if let Err(e) = std::fs::copy(self.fspath(from), self.fspath(to)) {
            debug!("copy failed: {:?}", e);
            return Err(e.into());
        }
        Ok(())
    }
}

impl DavReadDir for LocalFsReadDir {}

impl Iterator for LocalFsReadDir {
    type Item = Box<DavDirEntry>;

    fn next(&mut self) -> Option<Box<DavDirEntry>> {
        let entry = match self.iterator.next() {
            Some(Ok(e)) => e,
            Some(Err(_)) => { return None },
            None => { return None },
        };
        Some(Box::new(LocalFsDirEntry{
            name:   entry.file_name().as_bytes().to_vec(),
            entry:  entry,
        }))
    }
}

impl DavDirEntry for LocalFsDirEntry {
    fn metadata(&self) -> FsResult<Box<DavMetaData>> {
        match self.entry.metadata() {
            Ok(meta) => Ok(Box::new(LocalFsMetaData(meta))),
            Err(e) => Err(e.into()),
        }
    }

    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }

    fn is_dir(&self) -> FsResult<bool> { Ok(self.entry.file_type()?.is_dir()) }
    fn is_file(&self) -> FsResult<bool> { Ok(self.entry.file_type()?.is_file()) }
    fn is_symlink(&self) -> FsResult<bool> { Ok(self.entry.file_type()?.is_symlink())}
}

impl DavFile for LocalFsFile {
    fn metadata(&self) -> FsResult<Box<DavMetaData>> {
        let meta = self.0.metadata()?;
        Ok (Box::new(LocalFsMetaData(meta)))
    }
}

impl Read for LocalFsFile {
    fn read(&mut self, mut buf: &mut [u8]) -> IoResult<usize> {
        self.0.read(&mut buf)
    }
}

impl Write for LocalFsFile {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> IoResult<()> {
        self.0.flush()
    }
}

impl Seek for LocalFsFile {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        self.0.seek(pos)
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
        format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            res[0], res[1], res[2], res[3], res[4],
            res[5], res[6], res[7], res[8], res[9])
    }
}

impl From<std::io::Error> for FsError {
    fn from(e: std::io::Error) -> Self {

        if let Some(errno) = e.raw_os_error() {
            // specific errors.
            match errno {
                libc::EMLINK |
                libc::ENOSPC |
                libc::EDQUOT => return FsError::InsufficientStorage,
                libc::EFBIG => return FsError::TooLarge,
                libc::EACCES |
                libc::EPERM =>  return FsError::Forbidden,
                libc::ENOTEMPTY |
                libc::EEXIST => return FsError::Exists,
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

