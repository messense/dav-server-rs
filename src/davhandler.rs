//
// This module contains the main entry point of the library,
// DavHandler.
//
use std::error::Error as StdError;
use std::io;
use std::sync::Arc;

use bytes::{self, buf::Buf};
use derivative::Derivative;
use futures_util::stream::Stream;
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};
use http_body::Body as HttpBody;
use http_body_util::BodyExt;

use crate::body::{Body, StreamBody};
use crate::davheaders;
use crate::davpath::DavPath;
use crate::util::{dav_method, DavMethod, DavMethodSet};

use crate::errors::DavError;
use crate::fs::*;
use crate::ls::*;
use crate::voidfs::{is_voidfs, VoidFs};
use crate::DavResult;

/// WebDAV request handler.
///
/// The [`new`](Self::new) and [`builder`](Self::builder) methods are used to instantiate a handler.
///
/// The [`handle`](Self::handle) and [`handle_with`](Self::handle_with) methods do the actual work.
///
/// Type parameter `C` represents credentials for file systems with access control.
#[derive(Clone, Derivative)]
#[derivative(Default(bound = ""))]
pub struct DavHandler<C = ()> {
    pub(crate) config: Arc<DavConfig<C>>,
}

/// Configuration of the handler.
#[derive(Clone, Derivative)]
#[derivative(Default(bound = ""))]
pub struct DavConfig<C = ()> {
    // Prefix to be stripped off when handling request.
    pub(crate) prefix: Option<String>,
    // Filesystem backend.
    pub(crate) fs: Option<Box<dyn GuardedFileSystem<C>>>,
    // Locksystem backend.
    pub(crate) ls: Option<Box<dyn DavLockSystem>>,
    // Set of allowed methods (None means "all methods")
    pub(crate) allow: Option<DavMethodSet>,
    // Principal is webdav speak for "user", used to give locks an owner (if a locksystem is
    // active).
    pub(crate) principal: Option<String>,
    // Hide symbolic links? `None` maps to `true`.
    pub(crate) hide_symlinks: Option<bool>,
    // Does GET on a directory return indexes.
    pub(crate) autoindex: Option<bool>,
    // index.html
    pub(crate) indexfile: Option<String>,
    // read buffer size in bytes
    pub(crate) read_buf_size: Option<usize>,
    // Does GET on a file return 302 redirect.
    pub(crate) redirect: Option<bool>,
}

impl<C> DavConfig<C> {
    /// Create a new configuration builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Use the configuration that was built to generate a [`DavHandler`].
    pub fn build_handler(self) -> DavHandler<C> {
        DavHandler {
            config: Arc::new(self),
        }
    }

    /// Prefix to be stripped off before translating the rest of
    /// the request path to a filesystem path.
    pub fn strip_prefix(self, prefix: impl Into<String>) -> Self {
        let mut this = self;
        this.prefix = Some(prefix.into());
        this
    }

    /// Set the filesystem to use.
    pub fn filesystem(self, fs: Box<dyn GuardedFileSystem<C>>) -> Self {
        let mut this = self;
        this.fs = Some(fs);
        this
    }

    /// Set the locksystem to use.
    pub fn locksystem(self, ls: Box<dyn DavLockSystem>) -> Self {
        let mut this = self;
        this.ls = Some(ls);
        this
    }

    /// Which methods to allow (default is all methods).
    pub fn methods(self, allow: DavMethodSet) -> Self {
        let mut this = self;
        this.allow = Some(allow);
        this
    }

    /// Set the name of the "webdav principal". This will be the owner of any created locks.
    pub fn principal(self, principal: impl Into<String>) -> Self {
        let mut this = self;
        this.principal = Some(principal.into());
        this
    }

    /// Hide symbolic links (default is true)
    pub fn hide_symlinks(self, hide: bool) -> Self {
        let mut this = self;
        this.hide_symlinks = Some(hide);
        this
    }

    /// Does a GET on a directory produce a directory index.
    pub fn autoindex(self, autoindex: bool) -> Self {
        let mut this = self;
        this.autoindex = Some(autoindex);
        this
    }

    /// Indexfile to show (index.html, usually).
    pub fn indexfile(self, indexfile: impl Into<String>) -> Self {
        let mut this = self;
        this.indexfile = Some(indexfile.into());
        this
    }

    /// Read buffer size in bytes
    pub fn read_buf_size(self, size: usize) -> Self {
        let mut this = self;
        this.read_buf_size = Some(size);
        this
    }

    pub fn redirect(self, redirect: bool) -> Self {
        let mut this = self;
        this.redirect = Some(redirect);
        this
    }

    fn merge(&self, new: Self) -> Self {
        Self {
            prefix: new.prefix.or_else(|| self.prefix.clone()),
            fs: new.fs.or_else(|| self.fs.clone()),
            ls: new.ls.or_else(|| self.ls.clone()),
            allow: new.allow.or(self.allow),
            principal: new.principal.or_else(|| self.principal.clone()),
            hide_symlinks: new.hide_symlinks.or(self.hide_symlinks),
            autoindex: new.autoindex.or(self.autoindex),
            indexfile: new.indexfile.or_else(|| self.indexfile.clone()),
            read_buf_size: new.read_buf_size.or(self.read_buf_size),
            redirect: new.redirect.or(self.redirect),
        }
    }
}

// The actual inner struct.
//
// At the start of the request, DavConfig is used to generate
// a DavInner struct. DavInner::handle then handles the request.
pub(crate) struct DavInner<C> {
    pub prefix: String,
    pub fs: Box<dyn GuardedFileSystem<C>>,
    pub ls: Option<Box<dyn DavLockSystem>>,
    pub allow: Option<DavMethodSet>,
    pub principal: Option<String>,
    pub hide_symlinks: Option<bool>,
    pub autoindex: Option<bool>,
    pub indexfile: Option<String>,
    pub read_buf_size: Option<usize>,
    pub redirect: Option<bool>,
    pub credentials: C,
}

impl<C: Clone + Send + Sync + 'static> DavHandler<C> {
    /// Create a new `DavHandler`.
    ///
    /// This returns a DavHandler with an empty configuration. That's only
    /// useful if you use the `handle_with` method instead of `handle`.
    /// Normally you should create a new `DavHandler` using `DavHandler::build`
    /// and configure at least the filesystem, and probably the strip_prefix.
    pub fn new() -> Self {
        Self {
            config: Default::default(),
        }
    }

    /// Return a configuration builder.
    pub fn builder() -> DavConfig<C> {
        DavConfig::new()
    }

    /// Process a WebDAV request to a file system with access control.
    pub async fn handle_guarded<ReqBody, ReqData, ReqError>(
        &self,
        req: Request<ReqBody>,
        credentials: C,
    ) -> Response<Body>
    where
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
    {
        let inner = DavInner::new(self.config.as_ref().clone(), credentials);
        inner.handle(req).await
    }
}

impl DavHandler {
    /// Process a WebDAV request to a file system without access control.
    pub async fn handle<ReqBody, ReqData, ReqError>(&self, req: Request<ReqBody>) -> Response<Body>
    where
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
    {
        let inner = DavInner::new(self.config.as_ref().clone(), ());
        inner.handle(req).await
    }

    /// Handle a webdav request, overriding parts of the config.
    ///
    /// For example, the `principal` can be set for this request.
    ///
    /// Or, the default config has no locksystem, and you pass in
    /// a fake locksystem (`FakeLs`) because this is a request from a
    /// windows or macos client that needs to see locking support.
    pub async fn handle_with<ReqBody, ReqData, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> Response<Body>
    where
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
    {
        let inner = DavInner::new(self.config.merge(config), ());
        inner.handle(req).await
    }

    /// Handles a request with a `Stream` body instead of a `HttpBody`.
    /// Used with webserver frameworks that have not
    /// opted to use the `http_body` crate just yet.
    #[doc(hidden)]
    pub async fn handle_stream<ReqBody, ReqData, ReqError>(
        &self,
        req: Request<ReqBody>,
    ) -> Response<Body>
    where
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: Stream<Item = Result<ReqData, ReqError>>,
    {
        let req = {
            let (parts, body) = req.into_parts();
            Request::from_parts(parts, StreamBody::new(body))
        };
        let inner = DavInner::new(self.config.as_ref().clone(), ());
        inner.handle(req).await
    }

    /// Handles a request with a `Stream` body instead of a `HttpBody`.
    #[doc(hidden)]
    pub async fn handle_stream_with<ReqBody, ReqData, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> Response<Body>
    where
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: Stream<Item = Result<ReqData, ReqError>>,
    {
        let req = {
            let (parts, body) = req.into_parts();
            Request::from_parts(parts, StreamBody::new(body))
        };
        let inner = DavInner::new(self.config.merge(config), ());
        inner.handle(req).await
    }
}

impl<C> DavInner<C>
where
    C: Clone + Send + Sync + 'static,
{
    pub fn new(cfg: DavConfig<C>, credentials: C) -> Self {
        let DavConfig {
            prefix,
            fs,
            ls,
            allow,
            principal,
            hide_symlinks,
            autoindex,
            indexfile,
            read_buf_size,
            redirect,
        } = cfg;
        Self {
            prefix: prefix.unwrap_or_default(),
            fs: fs.unwrap_or_else(|| VoidFs::<C>::new()),
            ls,
            allow,
            principal,
            hide_symlinks,
            autoindex,
            indexfile,
            read_buf_size,
            redirect,
            credentials,
        }
    }

    // helper.
    pub(crate) async fn has_parent<'a>(&'a self, path: &'a DavPath) -> bool {
        let p = path.parent();
        self.fs
            .metadata(&p, &self.credentials)
            .await
            .map(|m| m.is_dir())
            .unwrap_or(false)
    }

    // helper.
    pub(crate) fn path(&self, req: &Request<()>) -> DavPath {
        // This never fails (has been checked before)
        DavPath::from_uri_and_prefix(req.uri(), &self.prefix).unwrap()
    }

    // See if this is a directory and if so, if we have
    // to fixup the path by adding a slash at the end.
    pub(crate) fn fixpath(
        &self,
        res: &mut Response<Body>,
        path: &mut DavPath,
        meta: Box<dyn DavMetaData>,
    ) -> Box<dyn DavMetaData> {
        if meta.is_dir() && !path.is_collection() {
            path.add_slash();
            let newloc = path.with_prefix().as_url_string();
            res.headers_mut()
                .typed_insert(davheaders::ContentLocation(newloc));
        }
        meta
    }

    // drain request body and return length.
    pub(crate) async fn read_request<ReqBody, ReqData, ReqError>(
        &self,
        body: ReqBody,
        max_size: usize,
    ) -> DavResult<Vec<u8>>
    where
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let mut data = Vec::new();
        pin_utils::pin_mut!(body);

        while let Some(res) = body.frame().await {
            let mut data_frame = res.map_err(|_| {
                DavError::IoError(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "UnexpectedEof",
                ))
            })?;

            let Some(buf) = data_frame.data_mut() else {
                continue;
            };

            while buf.has_remaining() {
                if data.len() + buf.remaining() > max_size {
                    return Err(StatusCode::PAYLOAD_TOO_LARGE.into());
                }
                let b = buf.chunk();
                let l = b.len();
                data.extend_from_slice(b);
                buf.advance(l);
            }
        }
        Ok(data)
    }

    // internal dispatcher.
    async fn handle<ReqBody, ReqData, ReqError>(self, req: Request<ReqBody>) -> Response<Body>
    where
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let is_ms = req
            .headers()
            .get("user-agent")
            .and_then(|s| s.to_str().ok())
            .map(|s| s.contains("Microsoft"))
            .unwrap_or(false);

        // Turn any DavError results into a HTTP error response.
        match self.handle2(req).await {
            Ok(resp) => {
                debug!("== END REQUEST result OK");
                resp
            }
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
                    resp = resp
                        .header("Cache-Control", "no-store, no-cache, must-revalidate")
                        .header("Progma", "no-cache")
                        .header("Expires", "0")
                        .header("Vary", "*");
                }
                resp = resp.header("Content-Length", "0").status(err.statuscode());
                if err.must_close() {
                    resp = resp.header("connection", "close");
                }
                resp.body(Body::empty()).unwrap()
            }
        }
    }

    // internal dispatcher part 2.
    async fn handle2<ReqBody, ReqData, ReqError>(
        mut self,
        req: Request<ReqBody>,
    ) -> DavResult<Response<Body>>
    where
        ReqBody: HttpBody<Data = ReqData, Error = ReqError>,
        ReqData: Buf + Send + 'static,
        ReqError: StdError + Send + Sync + 'static,
    {
        let (req, body) = {
            let (parts, body) = req.into_parts();
            (Request::from_parts(parts, ()), body)
        };

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
            }
        };

        // See if method makes sense if we don't have a filesystem.
        if is_voidfs::<C>(&self.fs) {
            match method {
                DavMethod::Options => {
                    if self
                        .allow
                        .as_ref()
                        .map(|a| a.contains(DavMethod::Options))
                        .unwrap_or(true)
                    {
                        let mut a = DavMethodSet::none();
                        a.add(DavMethod::Options);
                        self.allow = Some(a);
                    }
                }
                _ => {
                    debug!("no filesystem: method not allowed on request {}", req.uri());
                    return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
                }
            }
        }

        // see if method is allowed.
        if let Some(ref a) = self.allow {
            if !a.contains(method) {
                debug!(
                    "method {} not allowed on request {}",
                    req.method(),
                    req.uri()
                );
                return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
            }
        }

        // make sure the request path is valid.
        let path = DavPath::from_uri_and_prefix(req.uri(), &self.prefix)?;

        // PUT is the only handler that reads the body itself. All the
        // other handlers either expected no body, or a pre-read Vec<u8>.
        let (body_strm, body_data) = match method {
            DavMethod::Put | DavMethod::Patch => (Some(body), Vec::new()),
            _ => (None, self.read_request(body, 65536).await?),
        };

        // Not all methods accept a body.
        match method {
            DavMethod::Put
            | DavMethod::Patch
            | DavMethod::PropFind
            | DavMethod::PropPatch
            | DavMethod::Lock => {}
            _ => {
                if !body_data.is_empty() {
                    return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE.into());
                }
            }
        }

        debug!("== START REQUEST {:?} {}", method, path);

        match method {
            DavMethod::Options => self.handle_options(&req).await,
            DavMethod::PropFind => self.handle_propfind(&req, &body_data).await,
            DavMethod::PropPatch => self.handle_proppatch(&req, &body_data).await,
            DavMethod::MkCol => self.handle_mkcol(&req).await,
            DavMethod::Delete => self.handle_delete(&req).await,
            DavMethod::Lock => self.handle_lock(&req, &body_data).await,
            DavMethod::Unlock => self.handle_unlock(&req).await,
            DavMethod::Head | DavMethod::Get => self.handle_get(&req).await,
            DavMethod::Copy | DavMethod::Move => self.handle_copymove(&req, method).await,
            DavMethod::Put | DavMethod::Patch => self.handle_put(&req, body_strm.unwrap()).await,
        }
    }
}
