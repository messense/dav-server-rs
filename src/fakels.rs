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
use std::future;
use std::time::{Duration, SystemTime};

use futures_util::FutureExt;
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
        &'_ self,
        path: &DavPath,
        principal: Option<&str>,
        owner: Option<&Element>,
        timeout: Option<Duration>,
        shared: bool,
        deep: bool,
    ) -> LsFuture<'_, Result<DavLock, DavLock>> {
        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let d = if deep { 'I' } else { '0' };
        let s = if shared { 'S' } else { 'E' };
        let token = format!("opaquetoken:{}/{}/{}", Uuid::new_v4().hyphenated(), d, s);

        let lock = DavLock {
            token,
            path: Box::new(path.clone()),
            principal: principal.map(|s| s.to_string()),
            owner: owner.map(|o| Box::new(o.clone())),
            timeout_at: Some(timeout_at),
            timeout: Some(timeout),
            shared,
            deep,
        };
        debug!("lock {} created", &lock.token);
        future::ready(Ok(lock)).boxed()
    }

    fn unlock(&'_ self, _path: &DavPath, _token: &str) -> LsFuture<'_, Result<(), ()>> {
        future::ready(Ok(())).boxed()
    }

    fn refresh(
        &'_ self,
        path: &DavPath,
        token: &str,
        timeout: Option<Duration>,
    ) -> LsFuture<'_, Result<DavLock, ()>> {
        debug!("refresh lock {token}");
        let v: Vec<&str> = token.split('/').collect();
        let deep = v.len() > 1 && v[1] == "I";
        let shared = v.len() > 2 && v[2] == "S";

        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let lock = DavLock {
            token: token.to_string(),
            path: Box::new(path.clone()),
            principal: None,
            owner: None,
            timeout_at: Some(timeout_at),
            timeout: Some(timeout),
            shared,
            deep,
        };
        future::ready(Ok(lock)).boxed()
    }

    fn check(
        &'_ self,
        _path: &DavPath,
        _principal: Option<&str>,
        _ignore_principal: bool,
        _deep: bool,
        _submitted_tokens: &[String],
    ) -> LsFuture<'_, Result<(), DavLock>> {
        future::ready(Ok(())).boxed()
    }

    fn discover(&'_ self, _path: &DavPath) -> LsFuture<'_, Vec<DavLock>> {
        future::ready(Vec::new()).boxed()
    }

    fn delete(&'_ self, _path: &DavPath) -> LsFuture<'_, Result<(), ()>> {
        future::ready(Ok(())).boxed()
    }
}
