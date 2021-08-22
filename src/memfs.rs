//! Simple in-memory filesystem.
//!
//! This implementation has state, so if you create a
//! new instance in a handler(), it will be empty every time.
//!
//! This means you have to create the instance once, using `MemFs::new`, store
//! it in your handler struct, and clone() it every time you pass
//! it to the DavHandler. As a MemFs struct is just a handle, cloning is cheap.
use std::collections::HashMap;
use std::io::{Error, ErrorKind, SeekFrom};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use bytes::{Buf, Bytes};
use futures_util::{
    future,
    future::{BoxFuture, FutureExt},
};
use http::StatusCode;

use crate::davpath::DavPath;
use crate::fs::*;
use crate::tree;

type Tree = tree::Tree<Vec<u8>, MemFsNode>;

/// Ephemeral in-memory filesystem.
#[derive(Debug)]
pub struct MemFs {
    tree: Arc<Mutex<Tree>>,
}

#[derive(Debug, Clone)]
enum MemFsNode {
    Dir(MemFsDirNode),
    File(MemFsFileNode),
}

#[derive(Debug, Clone)]
struct MemFsDirNode {
    props:  HashMap<String, DavProp>,
    mtime:  SystemTime,
    crtime: SystemTime,
}

#[derive(Debug, Clone)]
struct MemFsFileNode {
    props:  HashMap<String, DavProp>,
    mtime:  SystemTime,
    crtime: SystemTime,
    data:   Vec<u8>,
}

#[derive(Debug, Clone)]
struct MemFsDirEntry {
    mtime:  SystemTime,
    crtime: SystemTime,
    is_dir: bool,
    name:   Vec<u8>,
    size:   u64,
}

#[derive(Debug)]
struct MemFsFile {
    tree:    Arc<Mutex<Tree>>,
    node_id: u64,
    pos:     usize,
    append:  bool,
}

impl MemFs {
    /// Create a new "memfs" filesystem.
    pub fn new() -> Box<MemFs> {
        let root = MemFsNode::new_dir();
        Box::new(MemFs {
            tree: Arc::new(Mutex::new(Tree::new(root))),
        })
    }

    fn do_open(&self, tree: &mut Tree, path: &[u8], options: OpenOptions) -> FsResult<Box<dyn DavFile>> {
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
        Ok(Box::new(MemFsFile {
            tree:    self.tree.clone(),
            node_id: node_id,
            pos:     0,
            append:  options.append,
        }))
    }
}

impl Clone for MemFs {
    fn clone(&self) -> Self {
        MemFs {
            tree: Arc::clone(&self.tree),
        }
    }
}

impl DavFileSystem for MemFs {
    fn metadata<'a>(&'a self, path: &'a DavPath) -> FsFuture<Box<dyn DavMetaData>> {
        async move {
            let tree = &*self.tree.lock().unwrap();
            let node_id = tree.lookup(path.as_bytes())?;
            let meta = tree.get_node(node_id)?.as_dirent(path.as_bytes());
            Ok(Box::new(meta) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> FsFuture<FsStream<Box<dyn DavDirEntry>>>
    {
        async move {
            let tree = &*self.tree.lock().unwrap();
            let node_id = tree.lookup(path.as_bytes())?;
            if !tree.get_node(node_id)?.is_dir() {
                return Err(FsError::Forbidden);
            }
            let mut v: Vec<Box<dyn DavDirEntry>> = Vec::new();
            for (name, dnode_id) in tree.get_children(node_id)? {
                if let Ok(node) = tree.get_node(dnode_id) {
                    v.push(Box::new(node.as_dirent(&name)));
                }
            }
            let strm = futures_util::stream::iter(v.into_iter());
            Ok(Box::pin(strm) as FsStream<Box<dyn DavDirEntry>>)
        }
        .boxed()
    }

    fn open<'a>(&'a self, path: &'a DavPath, options: OpenOptions) -> FsFuture<Box<dyn DavFile>> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            self.do_open(tree, path.as_bytes(), options)
        }
        .boxed()
    }

    fn create_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            trace!("FS: create_dir {:?}", path);
            let tree = &mut *self.tree.lock().unwrap();
            let path = path.as_bytes();
            let parent_id = tree.lookup_parent(path)?;
            tree.add_child(parent_id, file_name(path), MemFsNode::new_dir(), false)?;
            tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
            Ok(())
        }
        .boxed()
    }

    fn remove_file<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let parent_id = tree.lookup_parent(path.as_bytes())?;
            let node_id = tree.lookup(path.as_bytes())?;
            tree.delete_node(node_id)?;
            tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
            Ok(())
        }
        .boxed()
    }

    fn remove_dir<'a>(&'a self, path: &'a DavPath) -> FsFuture<()> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let parent_id = tree.lookup_parent(path.as_bytes())?;
            let node_id = tree.lookup(path.as_bytes())?;
            tree.delete_node(node_id)?;
            tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
            Ok(())
        }
        .boxed()
    }

    fn rename<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node_id = tree.lookup(from.as_bytes())?;
            let parent_id = tree.lookup_parent(from.as_bytes())?;
            let dst_id = tree.lookup_parent(to.as_bytes())?;
            tree.move_node(node_id, dst_id, file_name(to.as_bytes()), true)?;
            tree.get_node_mut(parent_id)?.update_mtime(SystemTime::now());
            tree.get_node_mut(dst_id)?.update_mtime(SystemTime::now());
            Ok(())
        }
        .boxed()
    }

    fn copy<'a>(&'a self, from: &'a DavPath, to: &'a DavPath) -> FsFuture<()> {
        async move {
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
        .boxed()
    }

    fn have_props<'a>(&'a self, _path: &'a DavPath) -> BoxFuture<'a, bool> {
        future::ready(true).boxed()
    }

    fn patch_props<'a>(
        &'a self,
        path: &'a DavPath,
        mut patch: Vec<(bool, DavProp)>,
    ) -> FsFuture<Vec<(StatusCode, DavProp)>>
    {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node_id = tree.lookup(path.as_bytes())?;
            let node = tree.get_node_mut(node_id)?;
            let props = node.get_props_mut();

            let mut res = Vec::new();

            let patch = patch.drain(..).collect::<Vec<_>>();
            for (set, p) in patch.into_iter() {
                let prop = cloneprop(&p);
                let status = if set {
                    props.insert(propkey(&p.namespace, &p.name), p);
                    StatusCode::OK
                } else {
                    props.remove(&propkey(&p.namespace, &p.name));
                    // the below map was added to signify if the remove succeeded or
                    // failed. however it seems that removing non-existant properties
                    // always succeed, so just return success.
                    //  .map(|_| StatusCode::OK).unwrap_or(StatusCode::NOT_FOUND)
                    StatusCode::OK
                };
                res.push((status, prop));
            }
            Ok(res)
        }
        .boxed()
    }

    fn get_props<'a>(&'a self, path: &'a DavPath, do_content: bool) -> FsFuture<Vec<DavProp>> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node_id = tree.lookup(path.as_bytes())?;
            let node = tree.get_node(node_id)?;
            let mut res = Vec::new();
            for (_, p) in node.get_props() {
                res.push(if do_content { p.clone() } else { cloneprop(p) });
            }
            Ok(res)
        }
        .boxed()
    }

    fn get_prop<'a>(&'a self, path: &'a DavPath, prop: DavProp) -> FsFuture<Vec<u8>> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node_id = tree.lookup(path.as_bytes())?;
            let node = tree.get_node(node_id)?;
            let p = node
                .get_props()
                .get(&propkey(&prop.namespace, &prop.name))
                .ok_or(FsError::NotFound)?;
            Ok(p.xml.clone().ok_or(FsError::NotFound)?)
        }
        .boxed()
    }
}

// small helper.
fn propkey(ns: &Option<String>, name: &str) -> String {
    ns.to_owned().as_ref().unwrap_or(&"".to_string()).clone() + name
}

// small helper.
fn cloneprop(p: &DavProp) -> DavProp {
    DavProp {
        name:      p.name.clone(),
        namespace: p.namespace.clone(),
        prefix:    p.prefix.clone(),
        xml:       None,
    }
}

impl DavDirEntry for MemFsDirEntry {
    fn metadata<'a>(&'a self) -> FsFuture<Box<dyn DavMetaData>> {
        let meta = (*self).clone();
        Box::pin(future::ok(Box::new(meta) as Box<dyn DavMetaData>))
    }

    fn name(&self) -> Vec<u8> {
        self.name.clone()
    }
}

impl DavFile for MemFsFile {
    fn metadata<'a>(&'a mut self) -> FsFuture<Box<dyn DavMetaData>> {
        async move {
            let tree = &*self.tree.lock().unwrap();
            let node = tree.get_node(self.node_id)?;
            let meta = node.as_dirent(b"");
            Ok(Box::new(meta) as Box<dyn DavMetaData>)
        }
        .boxed()
    }

    fn read_bytes<'a>(&'a mut self, count: usize) -> FsFuture<Bytes> {
        async move {
            let tree = &*self.tree.lock().unwrap();
            let node = tree.get_node(self.node_id)?;
            let file = node.as_file()?;
            let curlen = file.data.len();
            let mut start = self.pos;
            let mut end = self.pos + count;
            if start > curlen {
                start = curlen
            }
            if end > curlen {
                end = curlen
            }
            let cnt = end - start;
            self.pos += cnt;
            Ok(Bytes::copy_from_slice(&file.data[start..end]))
        }
        .boxed()
    }

    fn write_bytes<'a>(&'a mut self, buf: Bytes) -> FsFuture<()> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node = tree.get_node_mut(self.node_id)?;
            let file = node.as_file_mut()?;
            if self.append {
                self.pos = file.data.len();
            }
            let end = self.pos + buf.len();
            if end > file.data.len() {
                file.data.resize(end, 0);
            }
            file.data[self.pos..end].copy_from_slice(&buf);
            self.pos = end;
            Ok(())
        }
        .boxed()
    }

    fn write_buf<'a>(&'a mut self, mut buf: Box<dyn Buf + Send>) -> FsFuture<()> {
        async move {
            let tree = &mut *self.tree.lock().unwrap();
            let node = tree.get_node_mut(self.node_id)?;
            let file = node.as_file_mut()?;
            if self.append {
                self.pos = file.data.len();
            }
            let end = self.pos + buf.remaining();
            if end > file.data.len() {
                file.data.resize(end, 0);
            }
            while buf.has_remaining() {
                let b = buf.chunk();
                let len = b.len();
                file.data[self.pos..self.pos + len].copy_from_slice(b);
                buf.advance(len);
                self.pos += len;
            }
            Ok(())
        }
        .boxed()
    }

    fn flush<'a>(&'a mut self) -> FsFuture<()> {
        future::ok(()).boxed()
    }

    fn seek<'a>(&'a mut self, pos: SeekFrom) -> FsFuture<u64> {
        async move {
            let (start, offset): (u64, i64) = match pos {
                SeekFrom::Start(npos) => {
                    self.pos = npos as usize;
                    return Ok(npos);
                },
                SeekFrom::Current(npos) => (self.pos as u64, npos),
                SeekFrom::End(npos) => {
                    let tree = &*self.tree.lock().unwrap();
                    let node = tree.get_node(self.node_id)?;
                    let curlen = node.as_file()?.data.len() as u64;
                    (curlen, npos)
                },
            };
            if offset < 0 {
                if -offset as u64 > start {
                    return Err(Error::new(ErrorKind::InvalidInput, "invalid seek").into());
                }
                self.pos = (start - (-offset as u64)) as usize;
            } else {
                self.pos = (start + offset as u64) as usize;
            }
            Ok(self.pos as u64)
        }
        .boxed()
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

impl MemFsNode {
    fn new_dir() -> MemFsNode {
        MemFsNode::Dir(MemFsDirNode {
            crtime: SystemTime::now(),
            mtime:  SystemTime::now(),
            props:  HashMap::new(),
        })
    }

    fn new_file() -> MemFsNode {
        MemFsNode::File(MemFsFileNode {
            crtime: SystemTime::now(),
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
            name:   name.to_vec(),
            mtime:  mtime,
            crtime: crtime,
            is_dir: is_dir,
            size:   size as u64,
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
        let mut segs: Vec<&[u8]> = path.split(|&c| c == b'/').filter(|s| s.len() > 0).collect();
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
    path.split(|&c| c == b'/')
        .filter(|s| s.len() > 0)
        .last()
        .unwrap_or(b"")
        .to_vec()
}
