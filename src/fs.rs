
use std;
use std::time::{SystemTime,UNIX_EPOCH};
use std::io::{Read,Write,Seek};
use std::fmt::Debug;
use std::collections::HashMap;

use webpath::WebPath;
use xmltree::Element;
use hyper::status::StatusCode;

macro_rules! notimplemented {
    ($method:expr) => {
        Err(FsError::NotImplemented)
    }
}

#[derive(Debug,Clone,Copy,PartialEq)]
pub enum FsError {
    NotImplemented,
    GeneralFailure,
    Exists,
    NotFound,
    Forbidden,
    InsufficientStorage,
    LoopDetected,
    PathTooLong,
    TooLarge,
    IsRemote,
}
pub type FsResult<T> = std::result::Result<T, FsError>;

pub trait DavFileSystem : Debug + Sync + Send {
    fn open(&self, path: &WebPath, options: OpenOptions) -> FsResult<Box<DavFile>>;
    fn read_dir(&self, path: &WebPath) -> FsResult<Box< DavReadDir<Item=Box<DavDirEntry>> >>;
    fn metadata(&self, path: &WebPath) -> FsResult<Box<DavMetaData>>;

    fn create_dir(&self, _path: &WebPath) -> FsResult<()> {
        notimplemented!("create_dir")
    }
    fn remove_dir(&self, _path: &WebPath) -> FsResult<()> {
        notimplemented!("remove_dir")
    }
    fn remove_file(&self, _path: &WebPath) -> FsResult<()> {
        notimplemented!("remove_file")
    }
    fn rename(&self, _from: &WebPath, _to: &WebPath) -> FsResult<()> {
        notimplemented!("rename")
    }

    // we could supply a default implementations of this.
    fn copy(&self, _from: &WebPath, _to: &WebPath) -> FsResult<()> {
        notimplemented!("copy")
    }

    // helper so that clone() works.
    fn box_clone(&self) -> Box<DavFileSystem>;
}

pub trait DavProps : Debug {
    fn patch_props(&mut self, path: &WebPath, set: &Element, del: &Element) -> FsResult<HashMap<StatusCode, Vec<Element>>>;
    fn get_props(&self, path: &WebPath) -> FsResult<Vec<Element>>;
}

// generic Clone, calls implementation-specific box_clone().
impl Clone for Box<DavFileSystem> {
    fn clone(&self) -> Box<DavFileSystem> {
        self.box_clone()
    }
}

pub trait DavReadDir : Iterator<Item=Box<DavDirEntry>> + Debug {
}

pub trait DavDirEntry: Debug {
    fn name(&self) -> Vec<u8>;
    fn metadata(&self) -> FsResult<Box<DavMetaData>>;

    // defaults. implementations can override this if their
    // metadata() method is expensive and there is a cheaper
    // way to provide the same info (e.g. windows/unix filesystems).
    fn is_dir(&self) -> FsResult<bool> { Ok(self.metadata()?.is_dir()) }
    fn is_file(&self) -> FsResult<bool> { Ok(self.metadata()?.is_file()) }
    fn is_symlink(&self) -> FsResult<bool> { Ok(self.metadata()?.is_symlink()) }
}

pub trait DavFile: Read + Write + Seek + Debug {
    fn metadata(&self) -> Box<DavMetaData>;
}

pub trait DavMetaData : Debug {

    fn len(&self) -> u64;
    fn modified(&self) -> FsResult<SystemTime>;
	fn is_dir(&self) -> bool;

    // default implementations.
    fn etag(&self) -> String {
		if let Ok(t) = self.modified() {
            if let Ok(t) = t.duration_since(UNIX_EPOCH) {
			    // apache style etag.
			    return format!("{:x}-{:x}", self.len(),
				    t.as_secs() * 1000000 + t.subsec_nanos() as u64 / 1000);
            }
		}
		format!("{:x}", self.len())
	}
	fn is_file(&self) -> bool {
		!self.is_dir()
	}
	fn is_symlink(&self) -> bool {
		false
	}

    fn accessed(&self) -> FsResult<SystemTime> {
        notimplemented!("access time")
    }
    fn created(&self) -> FsResult<SystemTime> {
        notimplemented!("creation time")
    }
    fn status_changed(&self) -> FsResult<SystemTime> {
        notimplemented!("status change time")
    }
    fn executable(&self) -> FsResult<bool> {
        notimplemented!("executable")
    }
}

#[derive(Debug,Clone,Copy)]
pub struct OpenOptions {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    pub create: bool,
    pub create_new: bool,
    pub public: bool,
}

impl OpenOptions {
    #[allow(dead_code)]
    pub fn new() -> OpenOptions {
        OpenOptions{
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            public: false,
        }
    }
    pub fn read() -> OpenOptions {
        OpenOptions{
            read: true,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            public: false,
        }
    }
    pub fn write() -> OpenOptions {
        OpenOptions{
            read: false,
            write: true,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            public: false,
        }
    }
}

impl std::error::Error for FsError {
    fn description(&self) -> &str {
        "DavFileSystem error"
    }
    fn cause(&self) -> Option<&std::error::Error> {
        None
    }
}

impl std::fmt::Display for FsError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

