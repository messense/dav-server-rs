//
// This module contains the main entry point of the library,
// DavHandler.
//
use std::error::Error as StdError;
use std::io;
use std::sync::Arc;

use bytes;

use futures::compat::Stream01CompatExt;
use futures::future::TryFutureExt;
use futures::stream::StreamExt;
use futures01;

use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::davheaders;
use crate::util::{dav_method, empty_body, notfound, AllowedMethods, Method};
use crate::webpath::WebPath;

use crate::errors::DavError;
use crate::fs::*;
use crate::ls::*;
use crate::{BoxedByteStream, DavResult};

/// The webdav handler struct.
#[derive(Clone)]
pub struct DavHandler {
    config: Arc<DavConfig>,
}

/// Configuration of the handler.
#[derive(Default)]
pub struct DavConfig {
    /// Prefix to be stripped off when handling request.
    pub prefix: Option<String>,
    /// Filesystem backend.
    pub fs: Option<Box<dyn DavFileSystem>>,
    /// Locksystem backend.
    pub ls: Option<Box<dyn DavLockSystem>>,
    /// Set of allowed methods (None means "all methods")
    pub allow: Option<AllowedMethods>,
    /// Principal is webdav speak for "user", used to give locks an owner (if a locksystem is
    /// active).
    pub principal: Option<String>,
    /// Hide symbolic links? `None` maps to `true`.
    pub hide_symlinks: Option<bool>,
}

// The actual inner struct.
//
// At the start of the request, DavConfig is used to generate
// a DavInner struct. DavInner::handle then handles the request.
pub(crate) struct DavInner {
    pub prefix:        String,
    pub fs:            Box<dyn DavFileSystem>,
    pub ls:            Option<Box<dyn DavLockSystem>>,
    pub allow:         Option<AllowedMethods>,
    pub principal:     Option<String>,
    pub hide_symlinks: Option<bool>,
}

impl From<DavConfig> for DavInner {
    fn from(cfg: DavConfig) -> Self {
        DavInner {
            prefix:        cfg.prefix.unwrap_or("".to_string()),
            fs:            cfg.fs.unwrap(),
            ls:            cfg.ls,
            allow:         cfg.allow,
            principal:     cfg.principal,
            hide_symlinks: cfg.hide_symlinks,
        }
    }
}

impl From<&DavConfig> for DavInner {
    fn from(cfg: &DavConfig) -> Self {
        DavInner {
            prefix:        cfg
                .prefix
                .as_ref()
                .map(|p| p.to_owned())
                .unwrap_or("".to_string()),
            fs:            cfg.fs.clone().unwrap(),
            ls:            cfg.ls.clone(),
            allow:         cfg.allow,
            principal:     cfg.principal.clone(),
            hide_symlinks: cfg.hide_symlinks.clone(),
        }
    }
}

impl Clone for DavInner {
    fn clone(&self) -> Self {
        DavInner {
            prefix:        self.prefix.clone(),
            fs:            self.fs.clone(),
            ls:            self.ls.clone(),
            allow:         self.allow.clone(),
            principal:     self.principal.clone(),
            hide_symlinks: self.hide_symlinks.clone(),
        }
    }
}

impl DavHandler {
    /// Create a new `DavHandler`.
    /// - `prefix`: URL prefix to be stripped off.
    /// - `fs:` The filesystem backend.
    /// - `ls:` Optional locksystem backend
    pub fn new(prefix: Option<&str>, fs: Box<dyn DavFileSystem>, ls: Option<Box<dyn DavLockSystem>>) -> DavHandler {
        let config = DavConfig {
            prefix:        prefix.map(|s| s.to_string()),
            fs:            Some(fs),
            ls:            ls,
            allow:         None,
            principal:     None,
            hide_symlinks: None,
        };
        DavHandler {
            config: Arc::new(config),
        }
    }

    /// Create a new `DavHandler` with a more detailed configuration.
    ///
    /// For example, pass in a specific `AllowedMethods` set.
    pub fn new_with(config: DavConfig) -> DavHandler {
        DavHandler {
            config: Arc::new(config),
        }
    }

    /// Handle a webdav request.
    ///
    /// Only one error kind is ever returned: `ErrorKind::BrokenPipe`. In that case we
    /// were not able to generate a response at all, and the server should just
    /// close the connection.
    pub fn handle<ReqBody, ReqError>(
        &self,
        req: Request<ReqBody>,
    ) -> impl futures01::Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: futures01::Stream<Item = bytes::Bytes, Error = ReqError> + Send,
        ReqError: StdError + Send + Sync + 'static,
    {
        if self.config.fs.is_none() {
            return futures01::future::Either::A(notfound());
        }
        let inner = DavInner::from(&*self.config);
        futures01::future::Either::B(inner.handle(req))
    }

    /// Handle a webdav request, overriding parts of the config.
    ///
    /// For example, the `principal` can be set for this request.
    ///
    /// Or, the default config has no locksystem, and you pass in
    /// a fake locksystem (`FakeLs`) because this is a request from a
    /// windows or osx client that needs to see locking support.
    pub fn handle_with<ReqBody, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> impl futures01::Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: futures01::Stream<Item = bytes::Bytes, Error = ReqError> + Send,
        ReqError: StdError + Send + Sync + 'static,
    {
        let orig = &*self.config;
        let newconf = DavConfig {
            prefix:        config.prefix.or(orig.prefix.clone()),
            fs:            config.fs.or(orig.fs.clone()),
            ls:            config.ls.or(orig.ls.clone()),
            allow:         config.allow.or(orig.allow.clone()),
            principal:     config.principal.or(orig.principal.clone()),
            hide_symlinks: config.hide_symlinks.or(orig.hide_symlinks.clone()),
        };
        if newconf.fs.is_none() {
            return futures01::future::Either::A(notfound());
        }
        let inner = DavInner::from(newconf);
        futures01::future::Either::B(inner.handle(req))
    }
}

impl DavInner {
    // helper.
    pub(crate) async fn has_parent<'a>(&'a self, path: &'a WebPath) -> bool {
        let p = path.parent();
        self.fs.metadata(&p).await.map(|m| m.is_dir()).unwrap_or(false)
    }

    // helper.
    pub(crate) fn path(&self, req: &Request<()>) -> WebPath {
        // This never fails (has been checked before)
        WebPath::from_uri(req.uri(), &self.prefix).unwrap()
    }

    // See if this is a directory and if so, if we have
    // to fixup the path by adding a slash at the end.
    pub(crate) fn fixpath(
        &self,
        res: &mut Response<BoxedByteStream>,
        path: &mut WebPath,
        meta: Box<dyn DavMetaData>,
    ) -> Box<dyn DavMetaData>
    {
        if meta.is_dir() && !path.is_collection() {
            path.add_slash();
            let newloc = path.as_url_string_with_prefix();
            res.headers_mut().typed_insert(davheaders::ContentLocation(newloc));
        }
        meta
    }

    // drain request body and return length.
    pub(crate) async fn read_request<'a, ReqBody, ReqError>(
        &'a self,
        body: ReqBody,
        max_size: usize,
    ) -> DavResult<Vec<u8>>
    where
        ReqBody: futures01::Stream<Item = bytes::Bytes, Error = ReqError> + Send + 'a,
        ReqError: StdError + Send + Sync + 'static,
    {
        let mut body = futures::compat::Compat01As03::new(body);
        let mut data = Vec::new();
        while let Some(res) = body.next().await {
            let chunk = res.map_err(|_| {
                DavError::IoError(io::Error::new(io::ErrorKind::UnexpectedEof, "UnexpectedEof"))
            })?;
            if data.len() + chunk.len() > max_size {
                return Err(StatusCode::PAYLOAD_TOO_LARGE.into());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    // internal dispatcher.
    fn handle<ReqBody, ReqError>(
        self,
        req: Request<ReqBody>,
    ) -> impl futures01::Future<Item = Response<BoxedByteStream>, Error = io::Error>
    where
        ReqBody: futures01::Stream<Item = bytes::Bytes, Error = ReqError> + Send,
        ReqError: StdError + Send + Sync + 'static,
    {
        let fut = async move {
            let (req, body) = {
                let (parts, body) = req.into_parts();
                (Request::from_parts(parts, ()), body)
            };

            let is_ms = req
                .headers()
                .get("user-agent")
                .and_then(|s| s.to_str().ok())
                .map(|s| s.contains("Microsoft"))
                .unwrap_or(false);

            // Turn any DavError results into a HTTP error response.
            match self.handle2(req, body).await {
                Ok(resp) => {
                    debug!("== END REQUEST result OK");
                    Ok(resp)
                },
                Err(err) => {
                    debug!("== END REQUEST result {:?}", err);
                    let mut resp = Response::builder();
                    if is_ms && err.statuscode() == StatusCode::NOT_FOUND {
                        // This is an attempt to convince Windows to not
                        // cache a 404 NOT_FOUND for 30-60 seconds.
                        //
                        // That is a problem since windows caches the NOT_FOUND in a
                        // case-insensitive way. So if "www" does not exist, but "WWW" does,
                        // and you do a "dir www" and then a "dir WWW" the second one
                        // will fail.
                        //
                        // Ofcourse the below is not sufficient. Fixes welcome.
                        resp.header("Cache-Control", "no-store, no-cache, must-revalidate");
                        resp.header("Progma", "no-cache");
                        resp.header("Expires", "0");
                        resp.header("Vary", "*");
                    }
                    resp.header("Content-Length", "0");
                    resp.status(err.statuscode());
                    if err.must_close() {
                        resp.header("connection", "close");
                    }
                    let resp = resp.body(empty_body()).unwrap();
                    Ok(resp)
                },
            }
        };
        Box::pin(fut).compat()
    }

    // internal dispatcher part 2.
    async fn handle2<ReqBody, ReqError>(
        self,
        req: Request<()>,
        body: ReqBody,
    ) -> DavResult<Response<BoxedByteStream>>
    where
        ReqBody: futures01::Stream<Item = bytes::Bytes, Error = ReqError> + Send,
        ReqError: StdError + Send + Sync + 'static,
    {
        // debug when running the webdav litmus tests.
        if log_enabled!(log::Level::Debug) {
            if let Some(t) = req.headers().typed_get::<davheaders::XLitmus>() {
                debug!("X-Litmus: {:?}", t);
            }
        }

        // translate HTTP method to Webdav method.
        let method = match dav_method(req.method()) {
            Ok(m) => m,
            Err(e) => {
                debug!("refusing method {} request {}", req.method(), req.uri());
                return Err(e);
            },
        };

        // see if method is allowed.
        if let Some(ref a) = self.allow {
            if !a.allowed(method) {
                debug!("method {} not allowed on request {}", req.method(), req.uri());
                return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
            }
        }

        // make sure the request path is valid.
        let path = WebPath::from_uri(req.uri(), &self.prefix)?;

        // PUT is the only handler that reads the body itself. All the
        // other handlers either expected no body, or a pre-read Vec<u8>.
        let (body_strm, body_data) = match method {
            Method::Put | Method::Patch => (Some(body), Vec::new()),
            _ => (None, self.read_request(body, 65536).await?),
        };

        // Not all methods accept a body.
        match method {
            Method::Put | Method::Patch | Method::PropFind | Method::PropPatch | Method::Lock => {},
            _ => {
                if body_data.len() > 0 {
                    return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE.into());
                }
            },
        }

        debug!("== START REQUEST {:?} {}", method, path);

        let res = match method {
            Method::Options => self.handle_options(req).await,
            Method::PropFind => self.handle_propfind(req, body_data).await,
            Method::PropPatch => self.handle_proppatch(req, body_data).await,
            Method::MkCol => self.handle_mkcol(req).await,
            Method::Delete => self.handle_delete(req).await,
            Method::Lock => self.handle_lock(req, body_data).await,
            Method::Unlock => self.handle_unlock(req).await,
            Method::Head | Method::Get => self.handle_get(req).await,
            Method::Put | Method::Patch => self.handle_put(req, body_strm.unwrap().compat()).await,
            Method::Copy | Method::Move => self.handle_copymove(req, method).await,
        };
        res
    }
}
