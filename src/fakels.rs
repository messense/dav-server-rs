//! Fake locksystem (to make Windows/macOS work).
//!
//! Several Webdav clients, like the ones on Windows and macOS, require just
//! basic functionality to mount the Webdav server in read-only mode. However
//! to be able to mount the Webdav server in read-write mode, they require the
//! Webdav server to have Webdav class 2 compliance - that means, LOCK/UNLOCK
//! support.
//!
//! In many cases, this is not actually important. A lot of the current Webdav
//! server implementations that are used to serve a filesystem just fake it:
//! LOCK/UNLOCK always succeed, checking for locktokens in
//! If: headers always succeeds, and nothing is every really locked.
//!
//! `FakeLs` implements such a fake locksystem.
use std::time::{Duration, SystemTime};

use uuid::Uuid;
use xmltree::Element;

use crate::davpath::DavPath;
use crate::ls::*;

/// Fake locksystem implementation.
#[derive(Debug, Clone)]
pub struct FakeLs {}

impl FakeLs {
    /// Create a new "fakels" locksystem.
    pub fn new() -> Box<FakeLs> {
        Box::new(FakeLs {})
    }
}

fn tm_limit(d: Option<Duration>) -> Duration {
    match d {
        None => Duration::new(120, 0),
        Some(d) => {
            if d.as_secs() > 120 {
                Duration::new(120, 0)
            } else {
                d
            }
        }
    }
}

impl DavLockSystem for FakeLs {
    fn lock(
        &self,
        path: &DavPath,
        principal: Option<&str>,
        owner: Option<&Element>,
        timeout: Option<Duration>,
        shared: bool,
        deep: bool,
    ) -> Result<DavLock, DavLock> {
        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let d = if deep { 'I' } else { '0' };
        let s = if shared { 'S' } else { 'E' };
        let token = format!("opaquetoken:{}/{}/{}", Uuid::new_v4().hyphenated(), d, s);

        let lock = DavLock {
            token,
            path: path.clone(),
            principal: principal.map(|s| s.to_string()),
            owner: owner.cloned(),
            timeout_at: Some(timeout_at),
            timeout: Some(timeout),
            shared,
            deep,
        };
        debug!("lock {} created", &lock.token);
        Ok(lock)
    }

    fn unlock(&self, _path: &DavPath, _token: &str) -> Result<(), ()> {
        Ok(())
    }

    fn refresh(
        &self,
        path: &DavPath,
        token: &str,
        timeout: Option<Duration>,
    ) -> Result<DavLock, ()> {
        debug!("refresh lock {}", token);
        let v: Vec<&str> = token.split('/').collect();
        let deep = v.len() > 1 && v[1] == "I";
        let shared = v.len() > 2 && v[2] == "S";

        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let lock = DavLock {
            token: token.to_string(),
            path: path.clone(),
            principal: None,
            owner: None,
            timeout_at: Some(timeout_at),
            timeout: Some(timeout),
            shared,
            deep,
        };
        Ok(lock)
    }

    fn check(
        &self,
        _path: &DavPath,
        _principal: Option<&str>,
        _ignore_principal: bool,
        _deep: bool,
        _submitted_tokens: Vec<&str>,
    ) -> Result<(), DavLock> {
        Ok(())
    }

    fn discover(&self, _path: &DavPath) -> Vec<DavLock> {
        Vec::new()
    }

    fn delete(&self, _path: &DavPath) -> Result<(), ()> {
        Ok(())
    }
}
