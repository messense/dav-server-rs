//! Definitions for the Request and Response bodies.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, Bytes};
use futures::{future, stream, stream::Stream};
use http::header::HeaderMap;
use http_body::Body as HttpBody;

use crate::async_stream::AsyncStream;

/// Body is returned by the webdav handler, and implements both `Stream`
/// and `http_body::Body`.
pub struct Body {
    pub(crate) inner: BodyType,
}

pub(crate) enum BodyType {
    Stream(Box<dyn Stream<Item = io::Result<Bytes>> + Send + Unpin + 'static>),
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
            BodyType::Stream(ref mut strm) => {
                // cannot use pin_mut! - doesn't work with references.
                let strm = unsafe { Pin::new_unchecked(strm) };
                strm.poll_next(cx)
            },
            BodyType::AsyncStream(ref mut strm) => {
                // cannot use pin_mut! - doesn't work with references.
                let strm = unsafe { Pin::new_unchecked(strm) };
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

macro_rules! into_body {
    ($type:ty) => {
        impl From<$type> for Body {
            fn from(t: $type) -> Body {
                Body {
                    inner: BodyType::Stream(Box::new(stream::once(future::ready(Ok(Bytes::from(t)))))),
                }
            }
        }
    };
}

into_body!(String);
//into_body!(&str);
into_body!(Bytes);

impl From<AsyncStream<Bytes, io::Error>> for Body {
    fn from(s: AsyncStream<Bytes, io::Error>) -> Body {
        Body {
            inner: BodyType::AsyncStream(s),
        }
    }
}

use pin_project::pin_project;

//
// A struct that contains a http_body::Body, and implements Stream.
//
#[pin_project]
pub(crate) struct InBody<B, Data, Error>
where
    Data: Buf + Send,
    Error: std::error::Error + Send + Sync + 'static,
    B: HttpBody<Data = Data, Error = Error>,
{
    #[pin]
    body: B,
}

impl<B, Data, Error> Stream for InBody<B, Data, Error>
where
    Data: Buf + Send,
    Error: std::error::Error + Send + Sync + 'static,
    B: HttpBody<Data = Data, Error = Error>,
{
    type Item = Result<Bytes, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match this.body.poll_data(cx) {
            Poll::Ready(Some(Ok(mut item))) => Poll::Ready(Some(Ok(item.to_bytes()))),
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(io::Error::new(io::ErrorKind::Other, e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<B, Data, Error> InBody<B, Data, Error>
where
    Data: Buf + Send,
    Error: std::error::Error + Send + Sync + 'static,
    B: HttpBody<Data = Data, Error = Error>,
{
    pub fn from(body: B) -> InBody<B, Data, Error> {
        InBody { body }
    }
}
