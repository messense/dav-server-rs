//! Placeholder filesystem. Returns FsError::NotImplemented on every method.
//!
use std::{any::Any, marker::PhantomData};

use crate::davpath::DavPath;
use crate::fs::*;

/// Placeholder filesystem.
#[derive(Clone, Debug)]
pub struct VoidFs<C: Clone = ()> {
    _marker: PhantomData<C>,
}

pub fn is_voidfs<C: Clone + Send + Sync + 'static>(fs: &dyn Any) -> bool {
    fs.is::<Box<VoidFs<C>>>()
}

impl<C: Clone + Send + Sync + 'static> VoidFs<C> {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            _marker: Default::default(),
        })
    }
}

impl<C: Clone + Send + Sync + 'static> GuardedFileSystem<C> for VoidFs<C> {
    fn metadata<'a>(
        &'a self,
        _path: &'a DavPath,
        _credentials: &C,
    ) -> FsFuture<Box<dyn DavMetaData>> {
        Box::pin(async { Err(FsError::NotImplemented) })
    }

    fn read_dir<'a>(
        &'a self,
        _path: &'a DavPath,
        _meta: ReadDirMeta,
        _credentials: &C,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>> {
        Box::pin(async { Err(FsError::NotImplemented) })
    }

    fn open<'a>(
        &'a self,
        _path: &'a DavPath,
        _options: OpenOptions,
        _credentials: &C,
    ) -> FsFuture<Box<dyn DavFile>> {
        Box::pin(async { Err(FsError::NotImplemented) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memfs::MemFs;

    #[test]
    fn test_is_void() {
        assert!(is_voidfs::<i32>(&VoidFs::<i32>::new()));
        assert!(is_voidfs::<()>(&VoidFs::<()>::new()));
        assert!(!is_voidfs::<()>(&MemFs::new()));
    }
}
