//! Adapters to use the standard `http` types with Actix.
//!
//! Using the adapters in this crate, it's easy to build a webdav
//! handler for actix:
//!
//! ```no_run
//! use webdav_handler::{DavHandler, actix::DavRequest, actix::DavResponse};
//! use actix_web::web;
//!
//! pub async fn dav_handler(req: DavRequest, davhandler: web::Data<DavHandler>) -> DavResponse {
//!     davhandler.handle(req.request).await.into()
//! }
//! ```
//!
use std::io;

use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::error::PayloadError;
use actix_web::{dev, Error, FromRequest, HttpRequest, HttpResponse};
use bytes::Bytes;
use futures_util::{future, Stream};
use pin_project::pin_project;

/// http::Request compatibility.
///
/// Wraps `http::Request<DavBody>` and implements `actix_web::FromRequest`.
pub struct DavRequest {
    pub request: http::Request<DavBody>,
    prefix:      Option<String>,
}

impl DavRequest {
    /// Returns the request path minus the tail.
    pub fn prefix(&self) -> Option<&str> {
        self.prefix.as_ref().map(|s| s.as_str())
    }
}

impl FromRequest for DavRequest {
    type Config = ();
    type Error = Error;
    type Future = future::Ready<Result<DavRequest, Error>>;

    fn from_request(req: &HttpRequest, payload: &mut dev::Payload) -> Self::Future {
        let mut builder = http::Request::builder()
            .method(req.method().to_owned())
            .uri(req.uri().to_owned())
            .version(req.version().to_owned());
        for (name, value) in req.headers().iter() {
            builder = builder.header(name, value);
        }
        let path = req.match_info().path();
        let tail = req.match_info().unprocessed();
        let prefix = match &path[..path.len() - tail.len()] {
            "" | "/" => None,
            x => Some(x.to_string()),
        };

        let body = DavBody { body: payload.take() };
        let stdreq = DavRequest {
            request: builder.body(body).unwrap(),
            prefix,
        };
        future::ready(Ok(stdreq))
    }
}

/// Body type for `DavRequest`.
///
/// It wraps actix's `PayLoad` and implements `http_body::Body`.
#[pin_project]
pub struct DavBody {
    #[pin]
    body: dev::Payload,
}

impl http_body::Body for DavBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        let this = self.project();
        match this.body.poll_next(cx) {
            Poll::Ready(Some(Ok(data))) => Poll::Ready(Some(Ok(data))),
            Poll::Ready(Some(Err(err))) => {
                Poll::Ready(Some(Err(match err {
                    PayloadError::Incomplete(Some(err)) => err,
                    PayloadError::Incomplete(None) => io::ErrorKind::BrokenPipe.into(),
                    PayloadError::Io(err) => err,
                    other => io::Error::new(io::ErrorKind::Other, format!("{:?}", other)),
                })))
            },
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context,
    ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }
}

/// `http::Response` compatibility.
///
/// Wraps `http::Response<dav_handler::body::Body>` and implements actix_web::Responder.
pub struct DavResponse(pub http::Response<crate::body::Body>);

impl From<http::Response<crate::body::Body>> for DavResponse {
    fn from(resp: http::Response<crate::body::Body>) -> DavResponse {
        DavResponse(resp)
    }
}

impl actix_web::Responder for DavResponse {

    fn respond_to(self, _req: &HttpRequest) -> HttpResponse {
        use crate::body::{Body, BodyType};

        let (parts, body) = self.0.into_parts();
        let mut builder = HttpResponse::build(parts.status);
        for (name, value) in parts.headers.into_iter() {
            builder.append_header((name.unwrap(), value));
        }
        // I noticed that actix-web returns an empty chunked body
        // (\r\n0\r\n\r\n) and _no_ Transfer-Encoding header on
        // a 204 statuscode. It's probably because of
        // builder.streaming(). So only use builder.streaming()
        // on actual streaming replies.
        let resp = match body.inner {
            BodyType::Bytes(None) => builder.body(""),
            BodyType::Bytes(Some(b)) => builder.body(b),
            BodyType::Empty => builder.body(""),
            b @ BodyType::AsyncStream(..) => builder.streaming(Body { inner: b }),
        };
        resp
    }
}
