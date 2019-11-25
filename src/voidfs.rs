//! Placeholder filesystem. Returns FsError::NotImplemented on every method.
//!

use crate::fs::*;
use crate::davpath::DavPath;
use std::any::Any;

/// Placeholder filesystem.
#[derive(Debug, Clone)]
pub struct VoidFs;

pub fn is_voidfs(fs: &dyn Any) -> bool {
    fs.is::<Box<VoidFs>>()
}

impl VoidFs {
    pub fn new() -> Box<VoidFs> {
        Box::new(VoidFs)
    }
}

impl DavFileSystem for VoidFs {
    fn metadata<'a>(&'a self, _path: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        Box::pin (async { Err(FsError::NotImplemented) })
    }

    fn read_dir<'a>(
        &'a self,
        _path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>>
    {
        Box::pin (async { Err(FsError::NotImplemented) })
    }

    fn open<'a>(&'a self, _path: &'a DavPath, _options: OpenOptions) -> FsFuture<Box<dyn DavFile>> {
        Box::pin (async { Err(FsError::NotImplemented) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memfs::MemFs;

    #[test]
    fn test_is_void() {
        assert!(is_voidfs(&VoidFs::new()));
        assert!(!is_voidfs(&MemFs::new()));
    }
}

