//! Utility module to handle the path part of an URL as a filesytem path.
//!
use std::error::Error;
use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::ffi::OsString;
#[cfg(target_family = "unix")]
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use mime_guess;
use percent_encoding as pct;

use crate::DavError;

// Encode all non-unreserved characters, except '/'.
// See RFC3986, and https://en.wikipedia.org/wiki/Percent-encoding .
const PATH_ENCODE_SET: &pct::AsciiSet = &pct::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~')
    .remove(b'/');

/// URL path, with hidden prefix.
#[derive(Clone)]
pub struct DavPath {
    fullpath: Vec<u8>,
    pfxlen:   Option<usize>,
}

/// Reference to DavPath, no prefix.
/// It's what you get when you `Deref` `DavPath`, and returned by `DavPath::with_prefix()`.
pub struct DavPathRef {
    fullpath: [u8],
}

impl std::fmt::Display for DavPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}", self.as_pathbuf().display())
    }
}

impl std::fmt::Debug for DavPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", &self.as_url_string_with_prefix_debug())
    }
}

/// Error returned by some of the DavPath methods.
#[derive(Debug)]
pub enum ParseError {
    /// cannot parse
    InvalidPath,
    /// outside of prefix
    PrefixMismatch,
    /// too many dotdots
    ForbiddenPath,
}

impl Error for ParseError {
    fn description(&self) -> &str {
        "DavPath parse error"
    }
    fn cause(&self) -> Option<&dyn Error> {
        None
    }
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl From<ParseError> for DavError {
    fn from(e: ParseError) -> Self {
        match e {
            ParseError::InvalidPath => DavError::InvalidPath,
            ParseError::PrefixMismatch => DavError::IllegalPath,
            ParseError::ForbiddenPath => DavError::ForbiddenPath,
        }
    }
}

// a decoded segment can contain any value except '/' or '\0'
fn valid_segment(src: &[u8]) -> Result<(), ParseError> {
    let mut p = pct::percent_decode(src);
    if p.any(|x| x == 0 || x == b'/') {
        return Err(ParseError::InvalidPath);
    }
    Ok(())
}

// encode path segment with user-defined ENCODE_SET
fn encode_path(src: &[u8]) -> Vec<u8> {
    pct::percent_encode(src, PATH_ENCODE_SET).to_string().into_bytes()
}

// make path safe:
// - raw path before decoding can contain only printable ascii
// - make sure path is absolute
// - remove query part (everything after ?)
// - merge consecutive slashes
// - process . and ..
// - decode percent encoded bytes, fail on invalid encodings.
// - do not allow NUL or '/' in segments.
fn normalize_path(rp: &[u8]) -> Result<Vec<u8>, ParseError> {
    // must consist of printable ASCII
    if rp.iter().any(|&x| x < 32 || x > 126) {
        Err(ParseError::InvalidPath)?;
    }

    // don't allow fragments. query part gets deleted.
    let mut rawpath = rp;
    if let Some(pos) = rawpath.iter().position(|&x| x == b'?' || x == b'#') {
        if rawpath[pos] == b'#' {
            Err(ParseError::InvalidPath)?;
        }
        rawpath = &rawpath[..pos];
    }

    // must start with "/"
    if rawpath.is_empty() || rawpath[0] != b'/' {
        Err(ParseError::InvalidPath)?;
    }

    // split up in segments
    let isdir = match rawpath.last() {
        Some(x) if *x == b'/' => true,
        _ => false,
    };
    let segments = rawpath.split(|c| *c == b'/');
    let mut v: Vec<&[u8]> = Vec::new();
    for segment in segments {
        match segment {
            b"." | b"" => {},
            b".." => {
                if v.len() < 2 {
                    return Err(ParseError::ForbiddenPath);
                }
                v.pop();
                v.pop();
            },
            s => {
                if let Err(e) = valid_segment(s) {
                    Err(e)?;
                }
                v.push(b"/");
                v.push(s);
            },
        }
    }
    if isdir || v.is_empty() {
        v.push(b"/");
    }
    Ok(v.iter().flat_map(|s| pct::percent_decode(s)).collect())
}

/// Comparision ignores any trailing slash, so /foo == /foo/
impl PartialEq for DavPath {
    fn eq(&self, rhs: &DavPath) -> bool {
        let mut a = self.fullpath.as_slice();
        if a.len() > 1 && a.ends_with(b"/") {
            a = &a[..a.len() - 1];
        }
        let mut b = rhs.fullpath.as_slice();
        if b.len() > 1 && b.ends_with(b"/") {
            b = &b[..b.len() - 1];
        }
        a == b
    }
}

impl DavPath {
    /// from URL encoded path
    pub fn new(src: &str) -> Result<DavPath, ParseError> {
        let path = normalize_path(src.as_bytes())?;
        Ok(DavPath {
            fullpath: path.to_vec(),
            pfxlen:   None,
        })
    }

    /// Set prefix.
    pub fn set_prefix(&mut self, prefix: &str) -> Result<(), ParseError> {
        let path = &mut self.fullpath;
        let prefix = prefix.as_bytes();
        if !path.starts_with(prefix) {
            return Err(ParseError::PrefixMismatch);
        }
        let mut pfxlen = prefix.len();
        if prefix.ends_with(b"/") {
            pfxlen -= 1;
            if path[pfxlen] != b'/' {
                return Err(ParseError::PrefixMismatch);
            }
        } else if path.len() == pfxlen {
            path.push(b'/');
        }
        self.pfxlen = Some(pfxlen);
        Ok(())
    }

    /// Return a DavPathRef that refers to the entire URL path with prefix.
    pub fn with_prefix(&self) -> &DavPathRef {
        DavPathRef::new(&self.fullpath)
    }

    /// from URL encoded path and non-encoded prefix.
    pub(crate) fn from_str_and_prefix(src: &str, prefix: &str) -> Result<DavPath, ParseError> {
        let path = normalize_path(src.as_bytes())?;
        let mut davpath = DavPath {
            fullpath: path.to_vec(),
            pfxlen:   None,
        };
        davpath.set_prefix(prefix)?;
        Ok(davpath)
    }

    /// from request.uri
    pub(crate) fn from_uri_and_prefix(uri: &http::uri::Uri, prefix: &str) -> Result<Self, ParseError> {
        match uri.path() {
            "*" => {
                Ok(DavPath {
                    fullpath: b"*".to_vec(),
                    pfxlen:   None,
                })
            },
            path if path.starts_with("/") => DavPath::from_str_and_prefix(path, prefix),
            _ => Err(ParseError::InvalidPath),
        }
    }

    /// from request.uri
    pub fn from_uri(uri: &http::uri::Uri) -> Result<Self, ParseError> {
        Ok(DavPath {
            fullpath: uri.path().as_bytes().to_vec(),
            pfxlen:   None,
        })
    }

    /// add a slash to the end of the path (if not already present).
    pub(crate) fn add_slash(&mut self) {
        if !self.is_collection() {
            self.fullpath.push(b'/');
        }
    }

    // add a slash
    pub(crate) fn add_slash_if(&mut self, b: bool) {
        if b && !self.is_collection() {
            self.fullpath.push(b'/');
        }
    }

    /// Add a segment to the end of the path.
    pub(crate) fn push_segment(&mut self, b: &[u8]) {
        if !self.is_collection() {
            self.fullpath.push(b'/');
        }
        self.fullpath.extend_from_slice(b);
    }

    // as URL encoded string, with prefix.
    pub(crate) fn as_url_string_with_prefix_debug(&self) -> String {
        let mut p = encode_path(self.get_path());
        if self.get_prefix().len() > 0 {
            let mut u = encode_path(self.get_prefix());
            u.extend_from_slice(b"[");
            u.extend_from_slice(&p);
            u.extend_from_slice(b"]");
            p = u;
        }
        std::string::String::from_utf8(p).unwrap()
    }

    // Return the prefix.
    fn get_prefix(&self) -> &[u8] {
        &self.fullpath[..self.pfxlen.unwrap_or(0)]
    }

    /// return the URL prefix.
    pub fn prefix(&self) -> &str {
        std::str::from_utf8(self.get_prefix()).unwrap()
    }

    /// Return the parent directory.
    pub fn parent(&self) -> DavPath {
        let mut segs = self
            .fullpath
            .split(|&c| c == b'/')
            .filter(|e| e.len() > 0)
            .collect::<Vec<&[u8]>>();
        segs.pop();
        if segs.len() > 0 {
            segs.push(b"");
        }
        segs.insert(0, b"");
        DavPath {
            pfxlen:   self.pfxlen,
            fullpath: segs.join(&b'/').to_vec(),
        }
    }
}

impl std::ops::Deref for DavPath {
    type Target = DavPathRef;

    fn deref(&self) -> &DavPathRef {
        let pfxlen = self.pfxlen.unwrap_or(0);
        DavPathRef::new(&self.fullpath[pfxlen..])
    }
}

impl DavPathRef {
    // NOTE: this is safe, it is what libstd does in std::path::Path::new(), see
    // https://github.com/rust-lang/rust/blob/6700e186883a83008963d1fdba23eff2b1713e56/src/libstd/path.rs#L1788
    fn new(path: &[u8]) -> &DavPathRef {
        unsafe { &*(path as *const [u8] as *const DavPathRef) }
    }

    /// as raw bytes, not encoded, no prefix.
    pub fn as_bytes(&self) -> &[u8] {
        self.get_path()
    }

    /// as OS specific Path. never ends in "/".
    pub fn as_pathbuf(&self) -> PathBuf {
        let mut b = self.get_path();
        if b.len() > 1 && b.ends_with(b"/") {
            b = &b[..b.len() - 1];
        }
        #[cfg(not(target_os = "windows"))]
        let os_string = OsStr::from_bytes(b).to_owned();
        #[cfg(target_os = "windows")]
        let os_string = OsString::from(String::from_utf8(b.to_vec()).unwrap());
        PathBuf::from(os_string)
    }

    /// as URL encoded string, with prefix.
    pub fn as_url_string(&self) -> String {
        let p = encode_path(self.get_path());
        std::string::String::from_utf8(p).unwrap()
    }

    /// is this a collection i.e. does the original URL path end in "/".
    pub fn is_collection(&self) -> bool {
        self.get_path().ends_with(b"/")
    }

    // non-public functions
    //

    // Return the path.
    fn get_path(&self) -> &[u8] {
        &self.fullpath
    }

    // is this a "star" request (only used with OPTIONS)
    pub(crate) fn is_star(&self) -> bool {
        self.get_path() == b"*"
    }

    /// as OS specific Path, relative (remove first slash)
    ///
    /// Used to `push()` onto a pathbuf.
    pub fn as_rel_ospath(&self) -> &Path {
        let spath = self.get_path();
        let mut path = if spath.len() > 0 { &spath[1..] } else { spath };
        if path.ends_with(b"/") {
            path = &path[..path.len() - 1];
        }
        #[cfg(not(target_os = "windows"))]
        let os_string = OsStr::from_bytes(path);
        #[cfg(target_os = "windows")]
        let os_string : &OsStr = std::str::from_utf8(path).unwrap().as_ref();
        Path::new(os_string)
    }

    // get parent.
    #[allow(dead_code)]
    pub fn parent(&self) -> &DavPathRef {
        let path = self.get_path();

        let mut end = path.len();
        while end > 0 {
            end -= 1;
            if path[end] == b'/' {
                if end == 0 {
                    end = 1;
                }
                break;
            }
        }
        DavPathRef::new(&path[..end])
    }

    /// The filename is the last segment of the path. Can be empty.
    pub fn file_name_bytes(&self) -> &[u8] {
        let segs = self
            .get_path()
            .split(|&c| c == b'/')
            .filter(|e| e.len() > 0)
            .collect::<Vec<&[u8]>>();
        if segs.len() > 0 {
            segs[segs.len() - 1]
        } else {
            b""
        }
    }

    /// The filename is the last segment of the path. Can be empty.
    pub fn file_name(&self) -> Option<&str> {
        let name = self.file_name_bytes();
        if name.is_empty() {
            None
        } else {
            std::str::from_utf8(name).ok()
        }
    }

    pub(crate) fn get_mime_type_str(&self) -> &'static str {
        let name = self.file_name_bytes();
        let d = name.rsplitn(2, |&c| c == b'.').collect::<Vec<&[u8]>>();
        if d.len() > 1 {
            if let Ok(ext) = std::str::from_utf8(d[0]) {
                if let Some(t) = mime_guess::from_ext(ext).first_raw() {
                    return t;
                }
            }
        }
        "application/octet-stream"
    }
}
