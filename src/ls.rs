//! Contains the structs and traits that define a `locksystem` backend.
//!
//! Note that the methods DO NOT return futures, they are synchronous.
//! This is because currently only two locksystems exist, `MemLs` and `FakeLs`.
//! Both of them do not do any I/O, all methods return instantly.
//!
//! If ever a locksystem gets built that does I/O (to a filesystem,
//! a database, or over the network) we'll need to revisit this.
//!
use crate::davpath::DavPath;
use std::fmt::Debug;
use std::pin::Pin;
use std::time::{Duration, SystemTime};

use dyn_clone::{clone_trait_object, DynClone};
use futures_util::Future;
use xmltree::Element;

/// Type of the locks returned by DavLockSystem methods.
#[derive(Debug, Clone)]
pub struct DavLock {
    /// Token.
    pub token: String,
    /// Path/
    pub path: DavPath,
    /// Principal.
    pub principal: Option<String>,
    /// Owner.
    pub owner: Option<Element>,
    /// When the lock turns stale (absolute).
    pub timeout_at: Option<SystemTime>,
    /// When the lock turns stale (relative).
    pub timeout: Option<Duration>,
    /// Shared.
    pub shared: bool,
    /// Deep.
    pub deep: bool,
}

pub type LsFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The trait that defines a locksystem.
pub trait DavLockSystem: Debug + Send + Sync + DynClone {
    /// Lock a node. Returns `Ok(new_lock)` if succeeded,
    /// or `Err(conflicting_lock)` if failed.
    fn lock(
        &self,
        path: &DavPath,
        principal: Option<&str>,
        owner: Option<&Element>,
        timeout: Option<Duration>,
        shared: bool,
        deep: bool,
    ) -> LsFuture<Result<DavLock, DavLock>>;

    /// Unlock a node. Returns `Ok(())` if succeeded, `Err (())` if failed
    /// (because lock doesn't exist)
    fn unlock(&self, path: &DavPath, token: &str) -> LsFuture<Result<(), ()>>;

    /// Refresh lock. Returns updated lock if succeeded.
    fn refresh(
        &self,
        path: &DavPath,
        token: &str,
        timeout: Option<Duration>,
    ) -> LsFuture<Result<DavLock, ()>>;

    /// Check if node is locked and if so, if we own all the locks.
    /// If not, returns as Err one conflicting lock.
    fn check(
        &self,
        path: &DavPath,
        principal: Option<&str>,
        ignore_principal: bool,
        deep: bool,
        submitted_tokens: Vec<&str>,
    ) -> LsFuture<Result<(), DavLock>>;

    /// Find and return all locks that cover a given path.
    fn discover(&self, path: &DavPath) -> LsFuture<Vec<DavLock>>;

    /// Delete all locks at this path and below (after MOVE or DELETE)
    fn delete(&self, path: &DavPath) -> LsFuture<Result<(), ()>>;
}

clone_trait_object! {DavLockSystem}
