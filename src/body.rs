//! Definitions for the Request and Response bodies.

use std::error::Error as StdError;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures_util::stream::Stream;
use http::header::HeaderMap;
use http_body::Body as HttpBody;

use crate::async_stream::AsyncStream;

/// Body is returned by the webdav handler, and implements both `Stream`
/// and `http_body::Body`.
pub struct Body {
    pub(crate) inner: BodyType,
}

pub(crate) enum BodyType {
    Bytes(Option<Bytes>),
    AsyncStream(AsyncStream<Bytes, io::Error>),
    Empty,
}

impl Body {
    /// Return an empty body.
    pub fn empty() -> Body {
        Body {
            inner: BodyType::Empty,
        }
    }
}

impl Stream for Body {
    type Item = io::Result<Bytes>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.inner {
            BodyType::Bytes(ref mut strm) => Poll::Ready(strm.take().map(|b| Ok(b))),
            BodyType::AsyncStream(ref mut strm) => {
                let strm = Pin::new(strm);
                strm.poll_next(cx)
            },
            BodyType::Empty => Poll::Ready(None),
        }
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        self.poll_next(cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }
}

impl From<String> for Body {
    fn from(t: String) -> Body {
        Body {
            inner: BodyType::Bytes(Some(Bytes::from(t))),
        }
    }
}

impl From<&str> for Body {
    fn from(t: &str) -> Body {
        Body {
            inner: BodyType::Bytes(Some(Bytes::from(t.to_string()))),
        }
    }
}

impl From<Bytes> for Body {
    fn from(t: Bytes) -> Body {
        Body {
            inner: BodyType::Bytes(Some(t)),
        }
    }
}

impl From<AsyncStream<Bytes, io::Error>> for Body {
    fn from(s: AsyncStream<Bytes, io::Error>) -> Body {
        Body {
            inner: BodyType::AsyncStream(s),
        }
    }
}

use pin_project::pin_project;

//
// A struct that contains a Stream, and implements http_body::Body.
//
#[pin_project]
pub(crate) struct StreamBody<B> {
    #[pin]
    body: B,
}

impl<ReqBody, ReqData, ReqError> HttpBody for StreamBody<ReqBody>
where
    ReqData: Buf + Send,
    ReqError: StdError + Send + Sync + 'static,
    ReqBody: Stream<Item = Result<ReqData, ReqError>>,
{
    type Data = ReqData;
    type Error = ReqError;

    fn poll_data(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Self::Data, Self::Error>>>
    {
        let this = self.project();
        this.body.poll_next(cx)
    }

    fn poll_trailers(
        self: Pin<&mut Self>,
        _cx: &mut Context,
    ) -> Poll<Result<Option<HeaderMap>, Self::Error>>
    {
        Poll::Ready(Ok(None))
    }
}

impl<ReqBody, ReqData, ReqError> StreamBody<ReqBody>
where
    ReqData: Buf + Send,
    ReqError: StdError + Send + Sync + 'static,
    ReqBody: Stream<Item = Result<ReqData, ReqError>>,
{
    pub fn new(body: ReqBody) -> StreamBody<ReqBody> {
        StreamBody { body }
    }
}
