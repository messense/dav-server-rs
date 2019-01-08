use std::io::{self, Read, Write};
use std::mem;
use std::sync::Mutex;

use futures::prelude::*;
use futures::{future, stream, sync::mpsc, sync::mpsc::channel};

use bytes::{BufMut, Bytes, BytesMut};
use http::header::HeaderMap;
use http::status::StatusCode;

use lazy_static::lazy_static;
use threadpool::ThreadPool;

// The reponses sent over the response channel from the closure
// executing on the threadpool.
enum RespItem {
    Head { status: StatusCode, headers: HeaderMap },
    Body(Bytes),
}

// Thread pool itself. FIXME: size?
lazy_static! {
    static ref THREAD_POOL: Mutex<ThreadPool> = Mutex::new(ThreadPool::new(8));
}

// Internal errors.
#[derive(Debug)]
enum Error {
    HttpError(http::Error),
    ReqBody(String),
    ReqProxy,
    ReqProxySend,
    RespProxy,
    BodyTooLarge,
}

/// This more-or-less mirrors hyper-0.10's struct Request. We'll refactor
/// it to be more like http::Request later.
pub(crate) struct Request {
    pub method:  http::Method,
    pub headers: http::HeaderMap,
    pub uri:     http::Uri,
    body_buf:    Bytes,
    body:        stream::Wait<mpsc::Receiver<Bytes>>,
}

impl Read for Request {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // check if data is available.
        // if not, read it from the stream.
        if self.body_buf.len() == 0 {
            match self.body.next() {
                Some(Ok(item)) => self.body_buf = item,
                _ => return Ok(0),
            }
        };

        // copy as much as we can in the buffer supplied.
        let left = self.body_buf.len();
        let max = if buf.len() > left { left } else { buf.len() };
        if max == 0 {
            return Ok(0);
        }
        let dst = buf.get_mut(0..max).unwrap();
        let chunk = self.body_buf.split_to(max);
        dst.copy_from_slice(&chunk);
        return Ok(max);
    }
}

/// This more-or-less mirrors hyper-0.10's struct Response, which fortunately
/// is mostly the same as the one from the http crate.
pub(crate) struct Response {
    response:    http::Response<()>,
    body_buf:    BytesMut,
    resp_stream: futures::sink::Wait<mpsc::Sender<RespItem>>,
    did_hdrs:   bool,
}

// This looks a lot like the hyper-0.10 API surface we use.
impl Response {
    fn new(resp_stream: mpsc::Sender<RespItem>) -> Response {
        Response {
            response:    http::Response::new(()),
            body_buf:    BytesMut::new(),
            resp_stream: resp_stream.wait(),
            did_hdrs:   false,
        }
    }

    #[allow(dead_code)]
    pub fn headers_mut(&mut self) -> &mut http::HeaderMap<http::header::HeaderValue> {
        self.response.headers_mut()
    }

    pub fn status_mut(&mut self) -> &mut http::StatusCode {
        self.response.status_mut()
    }

    fn send_head(&mut self) {
        let mut response = http::Response::new(());
        mem::swap(&mut self.response, &mut response);
        let (parts, _) = response.into_parts();
        self.resp_stream
            .send(RespItem::Head {
                status:  parts.status,
                headers: parts.headers,
            })
            .ok();
    }

    pub fn start(mut self) -> Self {
        self.send_head();
        self.body_buf.reserve(32768);
        self.did_hdrs = true;
        self
    }
}

// The hyper-0.10 API implements Write for Reponse.
impl Write for Response {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let r = self.body_buf.remaining_mut();
        let n = if buf.len() > r { r } else { buf.len() };
        self.body_buf.put_slice(&buf[0..n]);
        if n == r {
            self.flush()?;
        }
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        let b = self.body_buf.take();
        if b.len() > 0 {
            self.resp_stream
                .send(RespItem::Body(b.into()))
                .map_err(|_| io::ErrorKind::BrokenPipe)?;
        }
        Ok(())
    }
}

// make sure the response body is flushed.
impl Drop for Response {
    fn drop(&mut self) {
        if !self.did_hdrs {
            self.send_head();
        }
        self.flush().ok();
    }
}

/// A simple type alias for a boxed stream of <Bytes, io::Error>.
pub type BoxedByteStream = Box<Stream<Item = Bytes, Error = io::Error> + Send + 'static>;

/// main entry point.
pub(crate) fn handler<ReqBody, ReqError, OldHandler>(
    req: http::Request<ReqBody>,
    oldhandler: OldHandler,
) -> impl Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
where
    ReqBody: Stream<Item = Bytes, Error = ReqError> + 'static,
    ReqError: ToString,
    OldHandler: FnOnce(Request, Response) + Send + 'static,
{
    let (parts, body) = req.into_parts();
    let body = body.map_err(|e| Error::ReqBody(e.to_string()));

    // For put requests, we do not buffer or limit the body.
    if &parts.method == &http::Method::PUT {
        let fut = proxy_to_pool(parts, body, oldhandler);
        return future::Either::A(map_errors(fut));
    }

    // read the body.
    let fut = body
        // concatenate the body, max size 65536.
        .fold((Vec::new(), false), |(mut acc, mut overflow), x| {
            if !overflow {
                if acc.len() + x.len() <= 65536 {
                    acc.extend_from_slice(&x);
                } else {
                    overflow = true;
                }
            }
            future::ok::<_, Error>((acc, overflow))
        })
        // check if body was too large.
        .and_then(|(body, overflow)| {
            if overflow {
                Err(Error::BodyTooLarge)
            } else {
                Ok(body)
            }
        })
        .and_then(|body| proxy_to_pool(parts, stream::once(Ok(Bytes::from(body))), oldhandler));

    future::Either::B(map_errors(fut))
}

// Spawn a request on the pool, then proxy the request/response.
fn proxy_to_pool<ReqBody, OldHandler>(
    parts: http::request::Parts,
    body: ReqBody,
    oldhandler: OldHandler,
) -> impl Future<Item = http::Response<BoxedByteStream>, Error = Error>
where
    ReqBody: Stream<Item = Bytes, Error = Error> + 'static,
    OldHandler: FnOnce(Request, Response) + Send + 'static,
{
    let version = parts.version.clone();

    // spawn the request on the threadpool.
    let (reqbody, respbody) = spawn_on_pool(parts, oldhandler);

    // first send the request body.
    let reqbody = reqbody.sink_map_err(|e| {
        debug!("proxy_to_pool: request body forwarding error {:?}", e);
        // Map the error from the channel to one of our own errors.
        //
        // Unfortunately, we cannot match on mpsc::SendError because the
        // inner member is private. Not even SendError<_>(..) works ...
        let desc = {
            use std::error::Error;
            e.description()
        };
        if desc.starts_with("send failed") {
            Error::ReqProxySend
        } else {
            Error::ReqProxy
        }
    });
    let forward_request = body.forward(reqbody);

    // now receive the response.
    let fut = forward_request
        .then(|res| {
            // Ignore ReqProxySend, it means the worker has gone away-
            // but there might still be a Response in the return channel.
            match res {
                Ok(_) | Err(Error::ReqProxySend) => Ok(()),
                Err(e) => Err(e),
            }
        })
        .and_then(|_| respbody.into_future().map_err(|_| Error::RespProxy))
        .then(move |res| {
            // So the first item from the stream is response status + response headers.
            let (status, headers, strm) = match res {
                Ok((Some(RespItem::Head { status, headers }), strm)) => (status, headers, strm),
                _ => return Err(Error::RespProxy),
            };
            debug!("proxy_to_pool: read response from channel");

            // the rest of the stream is the response body, a stream of Bytes.
            let strm = strm
                .map(|item: RespItem| {
                    debug!("proxy_to_pool: read body RespItem");
                    match item {
                        RespItem::Body(item) => item,
                        _ => Bytes::new(),
                    }
                })
                .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "error reading response channel"));

            // build the response
            let mut builder = http::response::Builder::new();
            let resp_builder = builder.status(status).version(version);
            // add the body stream.
            let mut resp = match resp_builder.body::<BoxedByteStream>(Box::new(strm)) {
                Ok(r) => r,
                Err(e) => return Err(Error::HttpError(e)),
            };
            // finally the response headers.
            // (why is there no "headers()" method on the builder?)
            *resp.headers_mut() = headers;
            Ok(resp)
        });
    fut
}

// This function makes sure we only return a rust error if there is no way to send
// a HTTP error. For example, when we're already sending the body. In that case,
// io::ErrorKind::BrokenPipe is returned. In all other cases, the error is mapped
// to a valid HTTP error response.
fn map_errors<FutIn>(fut: FutIn) -> impl Future<Item = http::Response<BoxedByteStream>, Error = io::Error>
where
    FutIn: Future<Item = http::Response<BoxedByteStream>, Error = Error>,
{
    fut.then(|res| {
        let (status, txt) = match res {
            Ok(resp) => return future::Either::A(future::ok(resp)),
            Err(Error::RespProxy) => {
                let err = io::Error::new(io::ErrorKind::BrokenPipe, "error reading response channel");
                return future::Either::A(future::err(err));
            },
            Err(Error::ReqProxySend) => (StatusCode::INTERNAL_SERVER_ERROR, "error writing request channel".into()),
            Err(Error::ReqProxy) => (StatusCode::INTERNAL_SERVER_ERROR, "error reading request body".into()),
            Err(Error::ReqBody(e)) => (StatusCode::BAD_REQUEST, format!("reading body: {}", e)),
            Err(Error::BodyTooLarge) => (StatusCode::PAYLOAD_TOO_LARGE, "request body too large".into()),
            Err(Error::HttpError(e)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("head: {}", e)),
        };
        let body = stream::once(Ok(Bytes::from(format!("<error>{}</error>\n", txt))));
        let body: BoxedByteStream = Box::new(body);
        let resp = http::Response::builder()
            .status(status)
            .header("content-type", "text/xml")
            .header("connection", "close")
            .body(body)
            .unwrap();
        future::Either::B(future::ok(resp))
    })
}

// Run the request synchronously on a thread in the thread pool.
fn spawn_on_pool<F>(
    parts: http::request::Parts,
    oldhandler: F,
) -> (mpsc::Sender<Bytes>, mpsc::Receiver<RespItem>)
where
    F: FnOnce(Request, Response) + Send + 'static,
{
    // First channel is used to send request body.
    let (req_tx, req_rx) = channel::<Bytes>(0);

    // Second channel is used to receive the reply.
    //
    // NOTE:
    // channel size zero actually means one item. First item sent is RespItem::Head,
    // that gets consumed immediately by the tokio side. Following items are
    // RespItem::Body, the first one of which will be buffered, any after that
    // will block until the tokio side reads from the stream.
    //
    // This is useful for short server replies (head + body item) so that the
    // thread from the pool can be reused immediately.
    let (resp_tx, resp_rx) = channel::<RespItem>(0);
    let pool = THREAD_POOL.lock().unwrap();
    pool.execute(move || {
        let req = Request {
            method:   parts.method,
            headers:  parts.headers,
            uri:      parts.uri,
            body:     req_rx.wait(),
            body_buf: Bytes::new(),
        };
        let resp = Response::new(resp_tx);
        info!("entering function on pool");
        oldhandler(req, resp);
        info!("exiting function on pool");
    });
    (req_tx, resp_rx)
}
