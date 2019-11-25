use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use actix_web::{web, dev, App, FromRequest, HttpMessage, HttpServer, HttpRequest, HttpResponse, Error};
use bytes::Bytes;
use futures::{future, Stream, stream::{StreamExt, TryStreamExt}};
use tokio_sync::mpsc;
use webdav_handler::{DavHandler, localfs::LocalFs, fakels::FakeLs};

struct StdHttpBody {
    rx:     mpsc::Receiver<Result<Bytes, io::Error>>,
}

impl StdHttpBody {
    fn new(payload: &mut dev::Payload) -> StdHttpBody {
        let body = web::Payload(payload.take());
        let (tx, rx) = mpsc::channel::new::<Result<bytes::Bytes, Error>>(0);
        tokio::spawn(async move {
            let _ = body.map(|res| match res {
                Ok(b) => Ok(b.into_buf()),
                Err(e) => Err(io::Error::from(e)),
            }).forward(&mut tx);
        });
        StdHttpBody{ rx }
    }
}

impl http_body::Body for StdHttpBody {
    type Data = io::Cursor<Bytes>;
    type Error = io::Error;

    fn poll_data(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Result<Self::Data, Self::Error>>> {
        match self.poll_next(cx) {
            Poll::Ready(Some(Ok(res))) => Poll::Ready(Some(res)),
            Poll::Ready(Some(Err(_))) => Poll::Ready(Some(Err(().into()))),
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

type StdHttpRequest = http::Request<StdHttpBody>;

impl FromRequest for StdHttpRequest
{
    type Config = ();
    type Error = Error;
    type Future = future::Ready<Result<StdHttpRequest, Error>>;

    fn from_request(req: &HttpRequest, payload: &mut dev::Payload) -> Self::Future {
        let mut builder = StdHttpRequest::builder();
        builder.method(req.method().to_owned());
        builder.uri(req.uri().to_owned());
        builder.version(req.version().to_owned());
        for (name, value) in req.headers().iter() {
            builder.header(name, value);
        }
        let mut body = payload.take();
        future::ready(Ok(builder.body(StdHttpBody::new(body)).unwrap()))
    }
}

async fn handler(req: StdHttpRequest, davhandler: web::Data<DavHandler>) -> Result<HttpResponse, Error> {
    davhandler.handle(req).await
}

fn main() -> std::io::Result<()> {
    let addr = ([127, 0, 0, 1], 4918).into();
    let dir = "/tmp";

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    HttpServer::new(|| App::with_state(dav_server).service(
        web::resource("/").to(handler)
    ))
        .bind("127.0.0.1:8080")?
        .run()
}
