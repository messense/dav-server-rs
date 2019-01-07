//! Fake locksystem (to make OSX/Windows work).
//!
//! LOCK/UNLOCK always succeed, checking for locktokens in
//! If: headers always succeeds, nothing is every really locked.
//!
//! This is enough for OSX/Windows to work without actually having
//! a working locksystem.
use std::time::{SystemTime,Duration};

use uuid::Uuid;
use xmltree::Element;

use webpath::WebPath;
use ls::*;

#[derive(Debug, Clone)]
pub struct FakeLs{}

impl FakeLs {
    /// Create a new "fakels" locksystem.
    pub fn new() -> Box<FakeLs> {
        Box::new(FakeLs{})
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

    fn lock(&self, path: &WebPath, owner: Option<&Element>, timeout: Option<Duration>, shared: bool, deep: bool) -> Result<DavLock, DavLock> {
        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let d = if deep { 'I' } else { '0' };
        let s = if shared { 'S' } else { 'E' };
        let token = format!("opaquetoken:{}/{}/{}", Uuid::new_v4().to_hyphenated(), d, s);

        let lock = DavLock{
            token:      token,
            path:       path.clone(),
            owner:      owner.cloned(),
            timeout_at: Some(timeout_at),
            timeout:    Some(timeout),
            shared:     shared,
            deep:       deep,
        };
        debug!("lock {} created", &lock.token);
        Ok(lock)
    }

    fn unlock(&self, _path: &WebPath, _token: &str) -> Result<(), ()> {
        Ok(())
    }

    fn refresh(&self, path: &WebPath, token: &str, timeout: Option<Duration>) -> Result<DavLock, ()> {

        debug!("refresh lock {}", token);
        let v: Vec<&str> = token.split('/').collect();
        let deep = v.len() > 1 && v[1] == "I";
        let shared = v.len() > 2 && v[2] == "S";

        let timeout = tm_limit(timeout);
        let timeout_at = SystemTime::now() + timeout;

        let lock = DavLock{
            token:      token.to_string(),
            path:       path.clone(),
            owner:      None,
            timeout_at: Some(timeout_at),
            timeout:    Some(timeout),
            shared:     shared,
            deep:       deep,
        };
        Ok(lock)
    }

    fn check(&self, _path: &WebPath, _deep: bool, _submitted_tokens: Vec<&str>) -> Result<(), DavLock> {
        Ok(())
    }

    fn discover(&self, _path: &WebPath) -> Vec<DavLock> {
        Vec::new()
    }

    fn delete(&self, _path: &WebPath) -> Result<(), ()> {
        Ok(())
    }
}

