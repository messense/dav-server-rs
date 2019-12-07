use std::io;

use actix_web::{web, App, HttpServer, HttpResponse};
use webdav_handler::{DavHandler, localfs::LocalFs, fakels::FakeLs};

async fn handler(req: StdHttpRequest, davhandler: web::Data<DavHandler>) -> HttpResponse {
    //
    // It would be a lot easier if Actix implemented
    // From<http::Response<Body>> for HttpResponse.
    //
    match davhandler.handle(req.request).await {
        Err(e) => HttpResponse::from_error(e.into()),
        Ok(resp) => {
            let (parts, body) = resp.into_parts();
            let mut builder = HttpResponse::build(parts.status);
            for (name, value) in parts.headers.into_iter() {
                builder.header(name.unwrap(), value);
            }
            builder.streaming(body)
        },
    }
}

fn main() -> io::Result<()> {
    let addr = "127.0.0.1:4918";
    let dir = "/tmp";

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    HttpServer::new(move || App::new().data(dav_server.clone()).service(
        web::resource("/").to(handler)
    ))
        .bind(addr)?
        .run()
}

//
// Adapters to use the standard `http` types with Actix.
//

use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::{dev, FromRequest, HttpRequest, Error};
use actix_web::client::PayloadError;
use bytes::Bytes;
use futures::{future, Stream};
use pin_project::pin_project;

/// Wrapper for Payload, so that we can implement http_body::Body.
#[pin_project]
struct StdHttpBody{
    #[pin]
    body: dev::Payload,
}

struct StdHttpRequest {
    request:    http::Request<StdHttpBody>,
}

///
/// Actix-web should really implement http_body::Body for Payload,
/// and std::error::Error for PayloadError.
///
impl http_body::Body for StdHttpBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        let this = self.project();
        match this.body.poll_next(cx) {
            Poll::Ready(Some(Ok(data))) => Poll::Ready(Some(Ok(data))),
            Poll::Ready(Some(Err(err))) => Poll::Ready(Some(Err(match err {
                PayloadError::Incomplete(Some(err)) => err,
                PayloadError::Incomplete(None) => io::ErrorKind::BrokenPipe.into(),
                PayloadError::Io(err) => err,
                other => io::Error::new(io::ErrorKind::Other, format!("{:?}", other)),
            }))),
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

///
/// Again, it would be cool if Actix simply implemented
/// FromRequest for http::Request<Payload>.
///
impl FromRequest for StdHttpRequest {
    type Config = ();
    type Error = Error;
    type Future = future::Ready<Result<StdHttpRequest, Error>>;

    fn from_request(req: &HttpRequest, payload: &mut dev::Payload) -> Self::Future {
        let mut builder = http::Request::builder()
            .method(req.method().to_owned())
            .uri(req.uri().to_owned())
            .version(req.version().to_owned());
        for (name, value) in req.headers().iter() {
            builder = builder.header(name, value);
        }
        let body = StdHttpBody{ body: payload.take() };
        let stdreq = StdHttpRequest{ request: builder.body(body).unwrap() };
        future::ready(Ok(stdreq))
    }
}

