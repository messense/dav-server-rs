//! Simple in-memory locksystem.
//!
//! This implementation has state - if you create a
//! new instance in a handler(), it will be empty every time.
//!
//! This means you have to create the instance once, using `MemLs::new`, store
//! it in your handler struct, and clone() it every time you pass
//! it to the DavHandler. As a MemLs struct is just a handle, cloning is cheap.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use uuid::Uuid;
use xmltree::Element;

use crate::davpath::DavPath;
use crate::fs::FsResult;
use crate::ls::*;
use crate::tree;

type Tree = tree::Tree<Vec<u8>, Vec<DavLock>>;

/// Ephemeral in-memory LockSystem.
#[derive(Debug, Clone)]
pub struct MemLs(Arc<Mutex<MemLsInner>>);

#[derive(Debug)]
struct MemLsInner {
    tree: Tree,
    #[allow(dead_code)]
    locks: HashMap<Vec<u8>, u64>,
}

impl MemLs {
    /// Create a new "memls" locksystem.
    pub fn new() -> Box<MemLs> {
        let inner = MemLsInner {
            tree: Tree::new(Vec::new()),
            locks: HashMap::new(),
        };
        Box::new(MemLs(Arc::new(Mutex::new(inner))))
    }
}

impl DavLockSystem for MemLs {
    fn lock(
        &self,
        path: &DavPath,
        principal: Option<&str>,
        owner: Option<&Element>,
        timeout: Option<Duration>,
        shared: bool,
        deep: bool,
    ) -> Result<DavLock, DavLock> {
        let inner = &mut *self.0.lock().unwrap();

        // any locks in the path?
        let rc = check_locks_to_path(&inner.tree, path, None, true, &Vec::new(), shared);
        trace!("lock: check_locks_to_path: {:?}", rc);
        rc?;

        // if it's a deep lock we need to check if there are locks furter along the path.
        if deep {
            let rc = check_locks_from_path(&inner.tree, path, None, true, &Vec::new(), shared);
            trace!("lock: check_locks_from_path: {:?}", rc);
            rc?;
        }

        // create lock.
        let node = get_or_create_path_node(&mut inner.tree, path);
        let timeout_at = timeout.map(|d| SystemTime::now() + d);
        let lock = DavLock {
            token: Uuid::new_v4().urn().to_string(),
            path: path.clone(),
            principal: principal.map(|s| s.to_string()),
            owner: owner.cloned(),
            timeout_at,
            timeout,
            shared,
            deep,
        };
        trace!("lock {} created", &lock.token);
        let slock = lock.clone();
        node.push(slock);
        Ok(lock)
    }

    fn unlock(&self, path: &DavPath, token: &str) -> Result<(), ()> {
        let inner = &mut *self.0.lock().unwrap();
        let node_id = match lookup_lock(&inner.tree, path, token) {
            None => {
                trace!("unlock: {} not found at {}", token, path);
                return Err(());
            }
            Some(n) => n,
        };
        let len = {
            let node = inner.tree.get_node_mut(node_id).unwrap();
            let idx = node.iter().position(|n| n.token.as_str() == token).unwrap();
            node.remove(idx);
            node.len()
        };
        if len == 0 {
            inner.tree.delete_node(node_id).ok();
        }
        Ok(())
    }

    fn refresh(
        &self,
        path: &DavPath,
        token: &str,
        timeout: Option<Duration>,
    ) -> Result<DavLock, ()> {
        trace!("refresh lock {}", token);
        let inner = &mut *self.0.lock().unwrap();
        let node_id = match lookup_lock(&inner.tree, path, token) {
            None => {
                trace!("lock not found");
                return Err(());
            }
            Some(n) => n,
        };
        let node = (&mut inner.tree).get_node_mut(node_id).unwrap();
        let idx = node.iter().position(|n| n.token.as_str() == token).unwrap();
        let lock = &mut node[idx];
        let timeout_at = timeout.map(|d| SystemTime::now() + d);
        lock.timeout = timeout;
        lock.timeout_at = timeout_at;
        Ok(lock.clone())
    }

    fn check(
        &self,
        path: &DavPath,
        principal: Option<&str>,
        ignore_principal: bool,
        deep: bool,
        submitted_tokens: Vec<&str>,
    ) -> Result<(), DavLock> {
        let inner = &*self.0.lock().unwrap();
        let _st = submitted_tokens.clone();
        let rc = check_locks_to_path(
            &inner.tree,
            path,
            principal,
            ignore_principal,
            &submitted_tokens,
            false,
        );
        trace!("check: check_lock_to_path: {:?}: {:?}", _st, rc);
        rc?;

        // if it's a deep lock we need to check if there are locks furter along the path.
        if deep {
            let rc = check_locks_from_path(
                &inner.tree,
                path,
                principal,
                ignore_principal,
                &submitted_tokens,
                false,
            );
            trace!("check: check_locks_from_path: {:?}", rc);
            rc?;
        }
        Ok(())
    }

    fn discover(&self, path: &DavPath) -> Vec<DavLock> {
        let inner = &*self.0.lock().unwrap();
        list_locks(&inner.tree, path)
    }

    fn delete(&self, path: &DavPath) -> Result<(), ()> {
        let inner = &mut *self.0.lock().unwrap();
        if let Some(node_id) = lookup_node(&inner.tree, path) {
            (&mut inner.tree).delete_subtree(node_id).ok();
        }
        Ok(())
    }
}

// check if there are any locks along the path.
fn check_locks_to_path(
    tree: &Tree,
    path: &DavPath,
    principal: Option<&str>,
    ignore_principal: bool,
    submitted_tokens: &[&str],
    shared_ok: bool,
) -> Result<(), DavLock> {
    // path segments
    let segs = path_to_segs(path, true);
    let last_seg = segs.len() - 1;

    // state
    let mut holds_lock = false;
    let mut first_lock_seen: Option<&DavLock> = None;

    // walk over path segments starting at root.
    let mut node_id = tree::ROOT_ID;
    for (i, seg) in segs.into_iter().enumerate() {
        node_id = match get_child(tree, node_id, seg) {
            Ok(n) => n,
            Err(_) => break,
        };
        let node_locks = match tree.get_node(node_id) {
            Ok(n) => n,
            Err(_) => break,
        };

        for nl in node_locks {
            if i < last_seg && !nl.deep {
                continue;
            }
            if submitted_tokens.iter().any(|t| &nl.token == t)
                && (ignore_principal || principal == nl.principal.as_deref())
            {
                // fine, we hold this lock.
                holds_lock = true;
            } else {
                // exclusive locks are fatal.
                if !nl.shared {
                    return Err(nl.to_owned());
                }
                // remember first shared lock seen.
                if !shared_ok {
                    first_lock_seen.get_or_insert(nl);
                }
            }
        }
    }

    // return conflicting lock on error.
    if !holds_lock {
        if let Some(first_lock_seen) = first_lock_seen {
            return Err(first_lock_seen.to_owned());
        }
    }

    Ok(())
}

// See if there are locks in any path below this collection.
fn check_locks_from_path(
    tree: &Tree,
    path: &DavPath,
    principal: Option<&str>,
    ignore_principal: bool,
    submitted_tokens: &[&str],
    shared_ok: bool,
) -> Result<(), DavLock> {
    let node_id = match lookup_node(tree, path) {
        Some(id) => id,
        None => return Ok(()),
    };
    check_locks_from_node(
        tree,
        node_id,
        principal,
        ignore_principal,
        submitted_tokens,
        shared_ok,
    )
}

// See if there are locks in any nodes below this node.
fn check_locks_from_node(
    tree: &Tree,
    node_id: u64,
    principal: Option<&str>,
    ignore_principal: bool,
    submitted_tokens: &[&str],
    shared_ok: bool,
) -> Result<(), DavLock> {
    let node_locks = match tree.get_node(node_id) {
        Ok(n) => n,
        Err(_) => return Ok(()),
    };
    for nl in node_locks {
        if (!nl.shared || !shared_ok)
            && (!submitted_tokens.iter().any(|t| t == &nl.token)
                || (!ignore_principal && principal != nl.principal.as_deref()))
        {
            return Err(nl.to_owned());
        }
    }
    if let Ok(children) = tree.get_children(node_id) {
        for (_, node_id) in children {
            if let Err(l) = check_locks_from_node(
                tree,
                node_id,
                principal,
                ignore_principal,
                submitted_tokens,
                shared_ok,
            ) {
                return Err(l);
            }
        }
    }
    Ok(())
}

// Find or create node.
fn get_or_create_path_node<'a>(tree: &'a mut Tree, path: &DavPath) -> &'a mut Vec<DavLock> {
    let mut node_id = tree::ROOT_ID;
    for seg in path_to_segs(path, false) {
        node_id = match tree.get_child(node_id, seg) {
            Ok(n) => n,
            Err(_) => tree
                .add_child(node_id, seg.to_vec(), Vec::new(), false)
                .unwrap(),
        };
    }
    tree.get_node_mut(node_id).unwrap()
}

// Find lock in path.
fn lookup_lock(tree: &Tree, path: &DavPath, token: &str) -> Option<u64> {
    trace!("lookup_lock: {}", token);

    let mut node_id = tree::ROOT_ID;
    for seg in path_to_segs(path, true) {
        trace!(
            "lookup_lock: node {} seg {}",
            node_id,
            String::from_utf8_lossy(seg)
        );
        node_id = match get_child(tree, node_id, seg) {
            Ok(n) => n,
            Err(_) => break,
        };
        let node = tree.get_node(node_id).unwrap();
        trace!("lookup_lock: locks here: {:?}", &node);
        if node.iter().any(|n| n.token == token) {
            return Some(node_id);
        }
    }
    trace!("lookup_lock: fail");
    None
}

// Find node ID for path.
fn lookup_node(tree: &Tree, path: &DavPath) -> Option<u64> {
    let mut node_id = tree::ROOT_ID;
    for seg in path_to_segs(path, false) {
        node_id = match tree.get_child(node_id, seg) {
            Ok(n) => n,
            Err(_) => return None,
        };
    }
    Some(node_id)
}

// Find all locks in a path
fn list_locks(tree: &Tree, path: &DavPath) -> Vec<DavLock> {
    let mut locks = Vec::new();

    let mut node_id = tree::ROOT_ID;
    if let Ok(node) = tree.get_node(node_id) {
        locks.extend_from_slice(node);
    }
    for seg in path_to_segs(path, false) {
        node_id = match tree.get_child(node_id, seg) {
            Ok(n) => n,
            Err(_) => break,
        };
        if let Ok(node) = tree.get_node(node_id) {
            locks.extend_from_slice(node);
        }
    }
    locks
}

fn path_to_segs(path: &DavPath, include_root: bool) -> Vec<&[u8]> {
    let path = path.as_bytes();
    let mut segs: Vec<&[u8]> = path
        .split(|&c| c == b'/')
        .filter(|s| !s.is_empty())
        .collect();
    if include_root {
        segs.insert(0, b"");
    }
    segs
}

fn get_child(tree: &Tree, node_id: u64, seg: &[u8]) -> FsResult<u64> {
    if seg.is_empty() {
        return Ok(node_id);
    }
    tree.get_child(node_id, seg)
}
