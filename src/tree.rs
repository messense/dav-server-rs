use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::Debug;
use std::hash::Hash;

use crate::FsError;
use crate::FsResult;

#[derive(Debug)]
/// A tree contains a bunch of nodes.
pub struct Tree<K: Eq + Hash, D> {
    nodes:   HashMap<u64, Node<K, D>>,
    node_id: u64,
}

/// id of the root node of the tree.
pub const ROOT_ID: u64 = 1;

#[derive(Debug)]
/// Node itself. "data" contains user-modifiable data.
pub struct Node<K: Eq + Hash, D> {
    pub data:  D,
    id:        u64,
    parent_id: u64,
    children:  HashMap<K, u64>,
}

#[derive(Debug)]
// Iterator over the children of a node.
pub struct Children<K>(std::vec::IntoIter<(K, u64)>);

impl<K: Eq + Hash + Debug + Clone, D: Debug> Tree<K, D> {
    /// Get new tree and initialize the root with 'data'.
    pub fn new(data: D) -> Tree<K, D> {
        let mut t = Tree {
            nodes:   HashMap::new(),
            node_id: ROOT_ID,
        };
        t.new_node(99999999, data);
        t
    }

    fn new_node(&mut self, parent: u64, data: D) -> u64 {
        let id = self.node_id;
        self.node_id += 1;
        let node = Node {
            id:        id,
            parent_id: parent,
            data:      data,
            children:  HashMap::new(),
        };
        self.nodes.insert(id, node);
        id
    }

    /// add a child node to an existing node.
    pub fn add_child(&mut self, parent: u64, key: K, data: D, overwrite: bool) -> FsResult<u64> {
        {
            let pnode = self.nodes.get(&parent).ok_or(FsError::NotFound)?;
            if !overwrite && pnode.children.contains_key(&key) {
                return Err(FsError::Exists);
            }
        }
        let id = self.new_node(parent, data);
        let pnode = self.nodes.get_mut(&parent).unwrap();

        pnode.children.insert(key, id);
        Ok(id)
    }

    /*
     * unused ...
    pub fn remove_child(&mut self, parent: u64, key: &K) -> FsResult<()> {
        let id = {
            let pnode = self.nodes.get(&parent).ok_or(FsError::NotFound)?;
            let id = *pnode.children.get(key).ok_or(FsError::NotFound)?;
            let node = self.nodes.get(&id).unwrap();
            if node.children.len() > 0 {
                return Err(FsError::Forbidden);
            }
            id
        };
        {
            let pnode = self.nodes.get_mut(&parent).unwrap();
            pnode.children.remove(key);
        }
        self.nodes.remove(&id);
        Ok(())
    }*/

    /// Get a child node by key K.
    pub fn get_child<Q: ?Sized>(&self, parent: u64, key: &Q) -> FsResult<u64>
    where
        K: Borrow<Q>,
        Q: Hash + Eq,
    {
        let pnode = self.nodes.get(&parent).ok_or(FsError::NotFound)?;
        let id = pnode.children.get(key).ok_or(FsError::NotFound)?;
        Ok(*id)
    }

    /// Get all children of this node. Returns an iterator over <K, D>.
    pub fn get_children(&self, parent: u64) -> FsResult<Children<K>> {
        let pnode = self.nodes.get(&parent).ok_or(FsError::NotFound)?;
        let mut v = Vec::new();
        for (k, i) in &pnode.children {
            v.push(((*k).clone(), *i));
        }
        Ok(Children(v.into_iter()))
    }

    /// Get reference to a node.
    pub fn get_node(&self, id: u64) -> FsResult<&D> {
        let n = self.nodes.get(&id).ok_or(FsError::NotFound)?;
        Ok(&n.data)
    }

    /// Get mutable reference to a node.
    pub fn get_node_mut(&mut self, id: u64) -> FsResult<&mut D> {
        let n = self.nodes.get_mut(&id).ok_or(FsError::NotFound)?;
        Ok(&mut n.data)
    }

    fn delete_node_from_parent(&mut self, id: u64) -> FsResult<()> {
        let parent_id = self.nodes.get(&id).ok_or(FsError::NotFound)?.parent_id;
        let key = {
            let pnode = self.nodes.get(&parent_id).unwrap();
            let mut key = None;
            for (k, i) in &pnode.children {
                if i == &id {
                    key = Some((*k).clone());
                    break;
                }
            }
            key
        };
        let key = key.unwrap();
        let pnode = self.nodes.get_mut(&parent_id).unwrap();
        pnode.children.remove(&key);
        Ok(())
    }

    /// Delete a node. Fails if node has children. Returns node itself.
    pub fn delete_node(&mut self, id: u64) -> FsResult<Node<K, D>> {
        {
            let n = self.nodes.get(&id).ok_or(FsError::NotFound)?;
            if n.children.len() > 0 {
                return Err(FsError::Forbidden);
            }
        }
        self.delete_node_from_parent(id)?;
        Ok(self.nodes.remove(&id).unwrap())
    }

    /// Delete a subtree.
    pub fn delete_subtree(&mut self, id: u64) -> FsResult<()> {
        let children = {
            let n = self.nodes.get(&id).ok_or(FsError::NotFound)?;
            n.children.iter().map(|(_, &v)| v).collect::<Vec<u64>>()
        };
        for c in children.into_iter() {
            self.delete_subtree(c)?;
        }
        self.delete_node_from_parent(id)
    }

    /// Move a node to a new position and new name in the tree.
    /// If "overwrite" is true, will replace an existing
    /// node, but only if it doesn't have any children.
    #[cfg(feature = "memfs")]
    pub fn move_node(&mut self, id: u64, new_parent: u64, new_name: K, overwrite: bool) -> FsResult<()> {
        let dest = {
            let pnode = self.nodes.get(&new_parent).ok_or(FsError::NotFound)?;
            if let Some(cid) = pnode.children.get(&new_name) {
                let cnode = self.nodes.get(cid).unwrap();
                if !overwrite || cnode.children.len() > 0 {
                    return Err(FsError::Exists);
                }
                Some(*cid)
            } else {
                None
            }
        };
        self.delete_node_from_parent(id)?;
        self.nodes.get_mut(&id).unwrap().parent_id = new_parent;
        if let Some(dest) = dest {
            self.nodes.remove(&dest);
        }
        let pnode = self.nodes.get_mut(&new_parent).unwrap();
        pnode.children.insert(new_name, id);
        Ok(())
    }
}

impl<K> Iterator for Children<K> {
    type Item = (K, u64);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next()
    }
}
