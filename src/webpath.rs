
use std;
use std::error::Error;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path,PathBuf};

use hyper;
use mime_guess;

use super::DavError;

#[derive(Debug,Clone,PartialEq)]
pub struct WebPath {
    path:       Vec<u8>,
    prefix:     Vec<u8>,
}

impl std::fmt::Display for WebPath {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}", &self.as_string())
    }
}

#[derive(Debug)]
pub enum ParseError {
    InvalidPath,
    IllegalPath,
}

impl Error for ParseError {
    fn description(&self) -> &str {
        "WebPath parse error"
    }
    fn cause(&self) -> Option<&Error> { None }
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
            ParseError::IllegalPath => DavError::IllegalPath,
        }
    }
}

struct PercentDecode<'a> {
    src:    &'a[u8],
    pos:    usize,
}

impl<'a> Iterator for PercentDecode<'a> {
    type Item = u8;
    fn next(&mut self) -> Option<Self::Item> {
        if self.pos == self.src.len() {
            return None;
        }
        let src = self.src;
        let i = self.pos;
        let c = src[i];
        if c == b'%' {
            if i >= src.len() - 2 {
                return None;
            }
            match (from_hexdigit(src[i+1]), from_hexdigit(src[i+2])) {
                (Some(c1), Some(c2)) => {
                    self.pos += 3;
                    return Some(c1 * 16 + c2);
                },
                _ => { return None; },
            };
        }
        self.pos += 1;
        Some(c)
    }
}

fn from_hexdigit(c: u8) -> Option<u8> {
    if c >= 48 && c <= 57 {
        return Some(c - 48);
    }
    if c >= 65 && c <= 70 {
        return Some(c - 55);
    }
    if c >= 97 && c <= 102 {
        return Some(c - 87);
    }
    None
}

fn to_hexdigit(c: u8) -> u8 {
    if c < 10 {
        return c + 48;
    }
    c + 55
}

/*
fn is_unreserved(c: u8) -> bool {
    (c >= 48 && c <= 57) ||
    (c >= 65 && c <= 90) ||
    (c >= 97 && c <= 122) ||
    c == '-' as u8 ||
    c == '_' as u8 ||
    c == '.' as u8 ||
    c == '~' as u8
}
*/

fn is_reserved(c: u8) -> bool {
    // control chars
    c < 33 ||
    // non-ascii
    c > 126 ||
    // set from js encodeURIcomponent, plus '#%', minus '/'.
    c == b';' ||
    c == b',' ||
    c == b'?' ||
    c == b':' ||
    c == b'@' ||
    c == b'&' ||
    c == b'=' ||
    c == b'+' ||
    c == b'$' ||
    c == b'#' ||
    c == b'%'
}

fn decode_path(src: &[u8]) -> PercentDecode {
    PercentDecode{
        src:    src,
        pos:    0,
    }
}

fn valid_segment(src: &[u8]) -> Result<(), ParseError> {
    let mut p = decode_path(src);
    if p.any(|x| x == 0 || x == b'/') || p.pos < p.src.len() {
        return Err(ParseError::InvalidPath);
    }
    Ok(())
}

fn encode_path(src: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    //for &c in src.into() {
    for &c in src {
        if is_reserved(c) {
            v.push(b'%');
            v.push(to_hexdigit(c / 16));
            v.push(to_hexdigit(c % 16));
        } else {
            v.push(c);
        }
    }
    v
}

// make path safe:
// - raw path before decoding can contain only printable ascii
// - make sure path is absolute
// - error on fragments (#)
// - remove everything after ?
// - merge consecutive slashes
// - handle . and ..
// - decode percent encoded bytes, fail on invalid encodings.
// - do not allow NUL or '/' in segments.
fn normalize_path(rp: &[u8]) -> Result<Vec<u8>, ParseError> {
    if rp.iter().any(|&x| x < 32 || x > 127 || x == b'#') {
        Err(ParseError::InvalidPath)?;
    }
    let mut rawpath = rp;
    if let Some(pos) = rawpath.iter().position(|&x| x == b'?') {
        rawpath = &rawpath[..pos];
    }
    if rawpath.is_empty() || rawpath[0] != b'/' {
        Err(ParseError::InvalidPath)?;
    }
    let isdir = match rawpath.last() {
        Some(x) if *x == b'/' => true,
        _ => false,
    };
    let segments = rawpath.split(|c| *c == b'/');
    let mut v : Vec<&[u8]>  = Vec::new();
    for segment in segments {
        match segment {
            b"." | b"" => {},
            b".." => { v.pop(); v.pop(); },
            s => {
                if let Err(e) = valid_segment(s) {
                    Err(e)?;
                }
                v.push(b"/");
                v.push(s);
            }
        }
    }
    if isdir || v.is_empty() {
        v.push(b"/");
    }
    Ok(v.iter().flat_map(|s| decode_path(s)).collect())
}

impl WebPath {
    // from an URL encoded string.
    pub fn from_str(src: &str, prefix: &str) -> Result<WebPath, ParseError> {
        let b = src.as_bytes();
        let path = normalize_path(b)?;
        let mut prefix = prefix.as_bytes();
        if !path.starts_with(prefix) {
            return Err(ParseError::IllegalPath);
        }
        let pflen = prefix.len();
        if prefix.ends_with(b"/") {
            prefix = &prefix[..pflen-1];
        } else if path.len() != pflen &&
                  (path.len() < pflen || path[pflen] != b'/') {
            return Err(ParseError::IllegalPath);
        }
        Ok(WebPath{
            path:   path[prefix.len()..].to_vec(),
            prefix: prefix.to_vec(),
        })
    }

    // from hyper req.uri
    pub fn from_uri(uri: &hyper::uri::RequestUri, prefix: &str) -> Result<Self, ParseError> {
        match uri {
            &hyper::uri::RequestUri::AbsolutePath(ref r) => {
                WebPath::from_str(r, prefix)
            },
            &hyper::uri::RequestUri::Star => {
                Ok(WebPath{
                    prefix: b"".to_vec(),
                    path: b"*".to_vec(),
                })
            },
            _ => {
                Err(ParseError::InvalidPath)
            }
        }
    }

    // from hyper Url
    pub fn from_url(url: &hyper::Url, prefix: &str) -> Result<Self, ParseError> {
        WebPath::from_str(url.path(), prefix)
    }

    pub fn is_star(&self) -> bool {
        self.path == b"*"
    }

    // as URL encoded string.
    pub fn as_string(&self) -> String {
        let p = encode_path(&self.path);
        std::string::String::from_utf8(p).unwrap()
    }

    // as URL encoded string.
    pub fn as_url_string(&self) -> String {
        let mut p = encode_path(&self.path);
        if self.prefix.len() > 0 {
            let mut u = encode_path(&self.prefix);
            u.extend_from_slice(&p);
            p = u;
        }
        std::string::String::from_utf8(p).unwrap()
    }

    // as OS specific Path.
    #[allow(dead_code)]
    pub fn as_pathbuf(&self) -> PathBuf {
        let mut b = self.path.as_slice();
        if b.len() > 1 && b.ends_with(b"/") {
            b = &b[..b.len()-1];
        }
        let os_string = OsStr::from_bytes(b).to_owned();
        PathBuf::from(os_string)
    }

    // as OS specific Path, relative (remove first slash)
    pub(crate) fn as_rel_pathbuf(&self) -> PathBuf {
        let mut path = if self.path.len() > 0 {
            &self.path[1..]
        } else {
            &self.path
        };
        if path.ends_with(b"/") {
            path = &path[..path.len()-1];
        }
        let os_string = OsStr::from_bytes(path).to_owned();
        PathBuf::from(os_string)
    }

    // does the path end in '/'
    pub fn is_collection(&self) -> bool {
        let l = self.path.len();
        l > 0 && self.path[l-1] == b'/'
    }

    // add a slash
    pub fn add_slash(&mut self) {
        if !self.is_collection() {
            self.path.push(b'/');
        }
    }

    // add a slash
    pub(crate) fn add_slash_if(&mut self, b: bool) {
        if b && !self.is_collection() {
            self.path.push(b'/');
        }
    }

    pub fn nth(&self, n: usize) -> Option<(&[u8])> {
        self.path.split(|c| *c == b'/').nth(n)
    }

    // prefix the WebPath with a Path and return a PathBuf
    pub(crate) fn as_pathbuf_with_prefix<P: AsRef<Path>>(&self, path: P) -> PathBuf {
        let mut p = path.as_ref().to_path_buf();
        p.push(self.as_rel_pathbuf());
        /*
        if self.is_collection() {
            p.push("");
        }
        */
        p
    }

    pub(crate) fn parent(&self) -> WebPath {
        let mut segs = self.path.split(|&c| c == b'/').filter(|e| e.len() > 0).collect::<Vec<&[u8]>>();
        segs.pop();
        if segs.len() > 0 {
            segs.push(b"");
        }
        segs.insert(0, b"");
        WebPath{
            prefix: self.prefix.clone(),
            path: segs.join(&b'/').to_vec(),
        }
    }

    pub(crate) fn file_name(&self) -> &[u8] {
        let segs = self.path.split(|&c| c == b'/').filter(|e| e.len() > 0).collect::<Vec<&[u8]>>();
        if segs.len() > 0 {
            segs[segs.len()-1]
        } else {
            b""
        }
    }

    pub(crate) fn push_segment(&mut self, b: &[u8]) {
        if !self.is_collection() {
            self.path.push(b'/');
        }
        self.path.extend_from_slice(b);
    }

    pub(crate) fn push_osstr<P: AsRef<OsStr>>(&mut self, path: P) {
        let b = path.as_ref().as_bytes();
        if !self.is_collection() {
            self.path.push(b'/');
        }
        self.path.extend_from_slice(b);
    }

    pub(crate) fn get_mime_type_str(&self) -> &'static str {
        let name = self.file_name();
        let d = name.rsplitn(2, |&c| c == b'.').collect::<Vec<&[u8]>>();
        if d.len() > 1 {
            if let Ok(ext) = std::str::from_utf8(d[0]) {
                if let Some(t) = mime_guess::get_mime_type_str(ext) {
                    return t;
                }
            }
        }
        "application/octet-stream"
    }
}

