//! Simple in-memory filesystem.
//!
//! This implementation has state so - if you create a
//! new instance in a handler(), it will be empty every time.
//!
//! So you have to create the instance once, using `MemFs::new`, store
//! it in your handler struct, and clone() it every time you pass
//! it to the DavHandler. Cloning is ofcourse not expensive, the
//! MemFs handle is refcounted, obviously.
use std;
use std::io::{self,Read,Write,Seek,SeekFrom};
use std::io::Result as IoResult;
use std::time::SystemTime;
use std::io::{Error,ErrorKind};
use std::sync::{Arc,Mutex};
use std::collections::HashMap;

use webpath::WebPath;
use hyper::status::StatusCode;

use fs::*;

use tree;

type Tree = tree::Tree<Vec<u8>, MemFsNode>;

#[derive(Debug)]
pub struct MemFs {
    tree:   Arc<Mutex<Tree>>,
}

#[derive(Debug,Clone)]
enum MemFsNode {
    Dir(MemFsDirNode),
    File(MemFsFileNode),
}

#[derive(Debug,Clone)]
struct MemFsDirNode {
    props:      HashMap<String, DavProp>,
    mtime:      SystemTime,
    crtime:     SystemTime,
}

#[derive(Debug,Clone)]
struct MemFsFileNode {
    props:      HashMap<String, DavProp>,
    mtime:      SystemTime,
    crtime:     SystemTime,
    data:       Vec<u8>,
}

#[derive(Debug)]
struct MemFsReadDir {
    iterator:   std::vec::IntoIter<MemFsDirEntry>
}

#[derive(Debug,Clone)]
struct MemFsDirEntry {
    mtime:      SystemTime,
    crtime:     SystemTime,
    is_dir:     bool,
    name:       Vec<u8>,
    size:       u64,
}

#[derive(Debug)]
struct MemFsFile {
    tree:       Arc<Mutex<Tree>>,
    node_id:    u64,
    pos:        usize,
    append:     bool,
}

impl MemFs {
    /// Create a new "memfs" filesystem.
    pub fn new() -> Box<MemFs> {
        let root = MemFsNode::new_dir();
        Box::new(MemFs{tree: Arc::new(Mutex::new(Tree::new(root)))})
    }

    fn do_open(&self, tree: &mut Tree, path: &[u8], options: OpenOptions) -> FsResult<Box<DavFile>> {
        let node_id = match tree.lookup(path) {
            Ok(n) => {
                if options.create_new {
                    return Err(FsError::Exists);
                }
                n
            },
            Err(FsError::NotFound) => {
                if !options.create {
                    return Err(FsError::NotFound);
                }
                let parent_id = tree.lookup_parent(path)?;
                tree.add_child(parent_id, file_name(path), MemFsNode::new_file(), true)?
            },
            Err(e) => return Err(e),
        };
        let node = tree.get_node_mut(node_id).unwrap();
        if node.is_dir() {
            return Err(FsError::Forbidden);
        }
        if options.truncate {
            node.as_file_mut()?.data.truncate(0);
            node.update_mtime(SystemTime::now());
        }
        Ok(Box::new(MemFsFile{
            tree:       self.tree.clone(),
            node_id:    node_id,
            pos:        0,
            append:     options.append,
        }))
    }
}

impl Clone for MemFs {
    fn clone(&self) -> Self {
        MemFs{
            tree: Arc::clone(&self.tree),
        }
    }
}

impl DavFileSystem for MemFs {

    fn metadata(&self, path: &WebPath) -> FsResult<Box<DavMetaData>> {
        let tree = &*self.tree.lock().unwrap();
        let node_id = tree.lookup(path.as_bytes())?;
        Ok(Box::new(tree.get_node(node_id)?.as_dirent(path.as_bytes())))
    }

    fn read_dir(&self, path: &WebPath) -> FsResult<Box<DavReadDir>> {
        let tree = &*self.tree.lock().unwrap();
        let node_id = tree.lookup(path.as_bytes())?;
        if !tree.get_node(node_id)?.is_dir() {
            return Err(FsError::Forbidden);
        }
        let mut v : Vec<MemFsDirEntry> = Vec::new();
        for (name, dnode_id) in tree.get_children(node_id)? {
            if let Ok(node) = tree.get_node(dnode_id) {
                v.push(node.as_dirent(&name));
            }
        }
        Ok(Box::new(MemFsReadDir{
            iterator: v.into_iter(),
        }))
    }

    fn open(&self, path: &WebPath, options: OpenOptions) -> FsResult<Box<DavFile>> {
        let tree = &mut *self.tree.lock().unwrap();
        self.do_open(tree, path.as_bytes(), options)
    }

    fn create_dir(&self, path: &WebPath) -> FsResult<()> {
        debug!("FS: create_dir {:?}", path);
        let tree = &mut *self.tree.lock().unwrap();
        let path = path.as_bytes();
        let parent_id = tree.lookup_parent(path)?;
        tree.add_child(parent_id, file_name(path), MemFsNode::new_dir(), false)?;
        tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
        Ok(())
    }

    fn remove_file(&self, path: &WebPath) -> FsResult<()> {
        let tree = &mut *self.tree.lock().unwrap();
        let parent_id = tree.lookup_parent(path.as_bytes())?;
        let node_id = tree.lookup(path.as_bytes())?;
        tree.delete_node(node_id)?;
        tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
        Ok(())
    }

    fn remove_dir(&self, path: &WebPath) -> FsResult<()> {
        let tree = &mut *self.tree.lock().unwrap();
        let parent_id = tree.lookup_parent(path.as_bytes())?;
        let node_id = tree.lookup(path.as_bytes())?;
        tree.delete_node(node_id)?;
        tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
        Ok(())
    }

    fn rename(&self, from: &WebPath, to: &WebPath) -> FsResult<()> {
        let tree = &mut *self.tree.lock().unwrap();
        let node_id = tree.lookup(from.as_bytes())?;
        let parent_id = tree.lookup_parent(from.as_bytes())?;
        let dst_id = tree.lookup_parent(to.as_bytes())?;
        tree.move_node(node_id, dst_id, file_name(to.as_bytes()), true)?;
        tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
        tree.get_node_mut(dst_id)?.update_mtime(SystemTime::now());
        Ok(())
    }

    fn copy(&self, from: &WebPath, to: &WebPath) -> FsResult<()> {
        let tree = &mut *self.tree.lock().unwrap();

        // source must exist.
        let snode_id = tree.lookup(from.as_bytes())?;

        // make sure destination exists, create if needed.
        {
            let mut oo = OpenOptions::write();
            oo.create = true;
            self.do_open(tree, to.as_bytes(), oo)?;
        }
        let dnode_id = tree.lookup(to.as_bytes())?;

        // copy.
        let mut data = (*tree.get_node_mut(snode_id)?).clone();
        match data {
            MemFsNode::Dir(ref mut d) => d.crtime = SystemTime::now(),
            MemFsNode::File(ref mut f) => f.crtime = SystemTime::now(),
        }
        *tree.get_node_mut(dnode_id)? = data;

        Ok(())
    }

    fn have_props(&self, _path: &WebPath) -> bool {
        true
    }

    fn patch_props(&self, path: &WebPath, set: Vec<DavProp>, remove: Vec<DavProp>) -> FsResult<Vec<(StatusCode, DavProp)>> {
        let tree = &mut *self.tree.lock().unwrap();
        let node_id = tree.lookup(path.as_bytes())?;
        let node = tree.get_node_mut(node_id)?;
        let props = node.get_props_mut();
        let mut res = Vec::new();
        for p in remove.into_iter() {
            props.remove(&propkey(&p.namespace, &p.name));
            res.push((StatusCode::Ok, p));
        }
        for p in set.into_iter() {
            res.push((StatusCode::Ok, cloneprop(&p)));
            props.insert(propkey(&p.namespace, &p.name), p);
        }
        Ok(res)
	}

    fn get_props(&self, path: &WebPath, do_content: bool) -> FsResult<Vec<DavProp>> {
        let tree = &mut *self.tree.lock().unwrap();
        let node_id = tree.lookup(path.as_bytes())?;
        let node = tree.get_node(node_id)?;
        let mut res = Vec::new();
        for (_, p) in node.get_props() {
            res.push(if do_content { p.clone() } else { cloneprop(p) });
        }
        Ok(res)
	}

    fn get_prop(&self, path: &WebPath, prop: DavProp) -> FsResult<Vec<u8>> {
        let tree = &mut *self.tree.lock().unwrap();
        let node_id = tree.lookup(path.as_bytes())?;
        let node = tree.get_node(node_id)?;
        let p = node.get_props().get(&propkey(&prop.namespace, &prop.name)).ok_or(FsError::NotFound)?;
        Ok(p.xml.clone().ok_or(FsError::NotFound)?)
    }
}

// small helper.
fn propkey(ns: &Option<String>, name: &str) -> String {
    ns.to_owned().as_ref().unwrap_or(&"".to_string()).clone() + name
}

// small helper.
fn cloneprop(p: &DavProp) -> DavProp {
    DavProp{ name: p.name.clone(), namespace: p.namespace.clone(), prefix: p.prefix.clone(), xml: None }
}

impl Iterator for MemFsReadDir {
    type Item = Box<DavDirEntry>;

    fn next(&mut self) -> Option<Box<DavDirEntry>> {
        match self.iterator.next() {
            Some(entry) => Some(Box::new(entry)),
            None => None,
        }
    }
}

impl DavDirEntry for MemFsDirEntry {

    fn metadata(&self) -> FsResult<Box<DavMetaData>> {
        Ok(Box::new((*self).clone()))
    }

    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }
}

impl DavMetaData for MemFsDirEntry {
    fn len(&self) -> u64 {
        self.size
    }

    fn created(&self) -> FsResult<SystemTime> {
        Ok(self.crtime)
    }

    fn modified(&self) -> FsResult<SystemTime> {
        Ok(self.mtime)
    }

    fn is_dir(&self) -> bool {
        self.is_dir
    }
}

impl DavFile for MemFsFile {
    fn metadata(&self) -> FsResult<Box<DavMetaData>> {
        let tree = &*self.tree.lock().unwrap();
        let node = tree.get_node(self.node_id)?;
        Ok(Box::new(node.as_dirent(b"")))
    }
}

impl Read for MemFsFile {
    fn read(&mut self, buf: &mut [u8]) -> IoResult<usize> {
        let tree = &*self.tree.lock().unwrap();
        let node = tree.get_node(self.node_id).map_err(fserror_to_ioerror)?;
        let file = node.as_file().map_err(fserror_to_ioerror)?;
        let curlen = file.data.len();
        let mut start = self.pos;
        let mut end = self.pos + buf.len();
        if start > curlen { start = curlen }
        if end > curlen { end = curlen }
        let cnt = end - start;
        buf[..cnt].copy_from_slice(&file.data[start..end]);
        Ok(cnt)
    }
}

impl Write for MemFsFile {
    fn write(&mut self, buf: &[u8]) -> IoResult<usize> {
        let tree = &mut *self.tree.lock().unwrap();
        let node = tree.get_node_mut(self.node_id).map_err(fserror_to_ioerror)?;
        let file = node.as_file_mut().map_err(fserror_to_ioerror)?;
        let start = if self.append { file.data.len() } else { self.pos };
        let end = start + buf.len();
        if end > file.data.len() {
            file.data.resize(end, 0);
        }
        file.data[start..end].copy_from_slice(buf);
        Ok(end - start)
    }

    fn flush(&mut self) -> IoResult<()> {
        Ok(())
    }
}

impl Seek for MemFsFile {
    fn seek(&mut self, pos: SeekFrom) -> IoResult<u64> {
        let (start, offset) : (u64, i64) = match pos {
            SeekFrom::Start(npos) => { self.pos = npos as usize; return Ok(npos) },
            SeekFrom::Current(npos) => (self.pos as u64, npos),
            SeekFrom::End(npos) => {
                let tree = &*self.tree.lock().unwrap();
                let node = tree.get_node(self.node_id).map_err(fserror_to_ioerror)?;
                let curlen = node.as_file().map_err(fserror_to_ioerror)?.data.len() as u64;
                (curlen, npos)
            },
        };
        if offset < 0 {
            if -offset as u64 > start {
                return Err(Error::new(ErrorKind::InvalidInput, "invalid seek"));
            }
            self.pos = (start - (-offset as u64)) as usize;
        } else {
            self.pos = (start + offset as u64) as usize;
        }
        Ok(self.pos as u64)
    }
}

impl MemFsNode {
    fn new_dir() -> MemFsNode {
        MemFsNode::Dir(MemFsDirNode{
            crtime:  SystemTime::now(),
            mtime:  SystemTime::now(),
            props:  HashMap::new(),
        })
    }

    fn new_file() -> MemFsNode {
        MemFsNode::File(MemFsFileNode{
            crtime:  SystemTime::now(),
            mtime:  SystemTime::now(),
            props:  HashMap::new(),
            data:   Vec::new(),
        })
    }

    // helper to create MemFsDirEntry from a node.
    fn as_dirent(&self, name: &[u8]) -> MemFsDirEntry {
        let (is_dir, size, mtime, crtime) = match self {
            &MemFsNode::File(ref file) => (false, file.data.len() as u64, file.mtime, file.crtime),
            &MemFsNode::Dir(ref dir) => (true, 0, dir.mtime, dir.crtime),
        };
        MemFsDirEntry {
            name: name.to_vec(),
            mtime: mtime,
            crtime: crtime,
            is_dir: is_dir,
            size: size as u64,
        }
    }

    fn update_mtime(&mut self, tm: std::time::SystemTime) {
        match self {
            &mut MemFsNode::Dir(ref mut d) => d.mtime = tm,
            &mut MemFsNode::File(ref mut f) => f.mtime = tm,
        }
    }

    fn is_dir(&self) -> bool {
        match self {
            &MemFsNode::Dir(_) => true,
            &MemFsNode::File(_) => false,
        }
    }

    fn as_file(&self) -> FsResult<&MemFsFileNode> {
        match self {
            &MemFsNode::File(ref n) => Ok(n),
            _ => Err(FsError::Forbidden),
        }
    }

    fn as_file_mut(&mut self) -> FsResult<&mut MemFsFileNode> {
        match self {
            &mut MemFsNode::File(ref mut n) => Ok(n),
            _ => Err(FsError::Forbidden),
        }
    }

    fn get_props(&self) -> &HashMap<String, DavProp> {
        match self {
            &MemFsNode::File(ref n) => &n.props,
            &MemFsNode::Dir(ref d) => &d.props,
        }
    }

    fn get_props_mut(&mut self) -> &mut HashMap<String, DavProp> {
        match self {
            &mut MemFsNode::File(ref mut n) => &mut n.props,
            &mut MemFsNode::Dir(ref mut d) => &mut d.props,
        }
    }
}

trait TreeExt {
    fn lookup_segs(&self, segs: Vec<&[u8]>) -> FsResult<u64>;
    fn lookup(&self, path: &[u8]) -> FsResult<u64>;
    fn lookup_parent(&self, path: &[u8]) -> FsResult<u64>;
}

impl TreeExt for Tree {
    fn lookup_segs(&self, segs: Vec<&[u8]>) -> FsResult<u64> {
        let mut node_id = tree::ROOT_ID;
        let mut is_dir = true;
        for seg in segs.into_iter() {
            if !is_dir {
                return Err(FsError::Forbidden);
            }
            if self.get_node(node_id)?.is_dir() {
                node_id = self.get_child(node_id, seg)?;
            } else {
                is_dir = false;
            }
        }
        Ok(node_id)
    }

    fn lookup(&self, path: &[u8]) -> FsResult<u64> {
        self.lookup_segs(path.split(|&c| c == b'/').filter(|s| s.len() > 0).collect())
    }

    // pop the last segment off the path, do a lookup, then
    // check if the result is a directory.
    fn lookup_parent(&self, path: &[u8]) -> FsResult<u64> {
        let mut segs : Vec<&[u8]> = path.split(|&c| c == b'/').filter(|s| s.len() > 0).collect();
        segs.pop();
        let node_id = self.lookup_segs(segs)?;
        if !self.get_node(node_id)?.is_dir() {
            return Err(FsError::Forbidden);
        }
        Ok(node_id)
    }
}

// helper
fn file_name(path: &[u8]) -> Vec<u8> {
    path.split(|&c| c == b'/').filter(|s| s.len() > 0).last().unwrap_or(b"").to_vec()
}

// error translation
fn fserror_to_ioerror(e: FsError) -> io::Error {
    match e {
        FsError::NotImplemented => io::Error::new(io::ErrorKind::Other, "NotImplemented"),
        FsError::GeneralFailure => io::Error::new(io::ErrorKind::Other, "GeneralFailure"),
        FsError::Exists => io::Error::new(io::ErrorKind::AlreadyExists, "Exists"),
        FsError::NotFound => io::Error::new(io::ErrorKind::NotFound, "Notfound"),
        FsError::Forbidden => io::Error::new(io::ErrorKind::PermissionDenied, "Forbidden"),
        FsError::InsufficientStorage => io::Error::new(io::ErrorKind::Other, "InsufficientStorage"),
        FsError::LoopDetected => io::Error::new(io::ErrorKind::Other, "LoopDetected"),
        FsError::PathTooLong => io::Error::new(io::ErrorKind::Other, "PathTooLong"),
        FsError::TooLarge => io::Error::new(io::ErrorKind::Other, "TooLarge"),
        FsError::IsRemote => io::Error::new(io::ErrorKind::Other, "IsRemote"),
    }
}

