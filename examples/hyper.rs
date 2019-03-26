use bytes::Bytes;
use futures01::{future::Future, stream::Stream};
use hyper;
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavHandler};

fn main() {
    env_logger::init();
    let dir = "/tmp";
    let addr = ([127, 0, 0, 1], 4918).into();

    let dav_server = DavHandler::new(None, LocalFs::new(dir, false, false), Some(FakeLs::new()));
    let make_service = move || {
        let dav_server = dav_server.clone();
        hyper::service::service_fn(move |req: hyper::Request<hyper::Body>| {
            let (parts, body) = req.into_parts();
            let body = body.map(|item| Bytes::from(item));
            let req = http::Request::from_parts(parts, body);
            let fut = dav_server.handle(req).and_then(|resp| {
                let (parts, body) = resp.into_parts();
                let body = hyper::Body::wrap_stream(body);
                Ok(hyper::Response::from_parts(parts, body))
            });
            Box::new(fut)
        })
    };

    println!("Serving {} on {}", dir, addr);
    let server = hyper::Server::bind(&addr)
        .serve(make_service)
        .map_err(|e| eprintln!("server error: {}", e));

    hyper::rt::run(server);
}
