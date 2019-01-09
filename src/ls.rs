//! Contains the structs and traits that define a "locksystem" backend.
//!
use std::time::{Duration,SystemTime};
use std::fmt::Debug;
use xmltree::Element;
use crate::webpath::WebPath;

/// Type of the locks returned by DavLockSystem methods.
#[derive(Debug,Clone)]
pub struct DavLock {
    pub token:      String,
    pub path:       WebPath,
    pub principal:  Option<String>,
    pub owner: 	    Option<Element>,
    pub timeout_at: Option<SystemTime>,
    pub timeout:    Option<Duration>,
    pub shared:     bool,
    pub deep:       bool,
}

/// The trait that defines a locksystem.
///
/// The BoxCloneLs trait is a helper trait that is automatically implemented
/// so that Box\<DavLockSystem\>.clone() works.
pub trait DavLockSystem : Debug + Sync + Send + BoxCloneLs {

    /// Lock a node. Returns Ok(new_lock) if succeeded,
	/// or Err(conflicting_lock) if failed.
    fn lock(&self, path: &WebPath, principal: Option<&str>, owner: Option<&Element>, timeout: Option<Duration>, shared: bool, deep: bool) -> Result<DavLock, DavLock>;

    /// Unlock a node. Returns empty Ok if succeeded, empty Err if failed
    /// (because lock doesn't exist)
    fn unlock(&self, path: &WebPath, token: &str) -> Result<(), ()> ;

    /// Refresh lock. Returns updated lock if succeeded.
    fn refresh(&self, path: &WebPath, token: &str, timeout: Option<Duration>) -> Result<DavLock, ()>;

    /// Check if node is locked and if so, if we own all the locks.
    /// If not, returns as Err one conflicting lock.
    fn check(&self, path: &WebPath, principal: Option<&str>, ignore_principal: bool, deep: bool, submitted_tokens: Vec<&str>) -> Result<(), DavLock>;

    /// Find and return all locks that cover a given path.
    fn discover(&self, path: &WebPath) -> Vec<DavLock>;

    /// Delete all locks at this path and below (after MOVE or DELETE)
    fn delete(&self, path: &WebPath) -> Result<(), ()>;
}

#[doc(hidden)]
pub trait BoxCloneLs {
    fn box_clone(&self) -> Box<DavLockSystem>;
}

// generic Clone, calls implementation-specific box_clone().
impl Clone for Box<DavLockSystem> {
    fn clone(&self) -> Box<DavLockSystem> {
        self.box_clone()
    }
}

// implementation-specific clone.
#[doc(hidden)]
impl<LS: Clone + DavLockSystem + 'static> BoxCloneLs for LS {
    fn box_clone(&self) -> Box<DavLockSystem> {
        Box::new((*self).clone())
    }
}
