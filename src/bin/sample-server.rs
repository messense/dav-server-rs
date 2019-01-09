//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/
//

use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;

#[macro_use]
extern crate clap;

use bytes::Bytes;
use env_logger;
use futures::{
    future::Future,
    stream::Stream,
};
use hyper;

use webdav_handler::{
    DavHandler,
    localfs,
    memfs,
    memls,
    typed_headers::{
        Authorization,
        Basic,
        HeaderMapExt,
    },
};

#[derive(Clone)]
struct Server {
    dh:     DavHandler,
    auth:   bool,
}

type BoxedFuture = Box<Future<Item=hyper::Response<hyper::Body>, Error=std::io::Error> + Send>;

impl Server {
    pub fn new(directory: String, auth: bool) -> Self {
        let dh = if directory != "" {
            let fs = localfs::LocalFs::new(directory, true);
            DavHandler::new("", None, fs, None)
        } else {
            let fs = memfs::MemFs::new();
            let ls = memls::MemLs::new();
            DavHandler::new("", None, fs, Some(ls))
        };
        Server{ dh, auth }
    }

    fn handle(&self, req: hyper::Request<hyper::Body>) -> BoxedFuture {

        let handler = if self.auth {
            // we want the client to authenticate.
            match req.headers().typed_get::<Authorization<Basic>>() {
                Some(Authorization(basic)) => {
                    self.dh.clone_with(None, Some(&basic.username), None, None)
                },
                None => {
                    // return a 401 reply.
                    let body = futures::stream::once(Ok(Bytes::from("please auth")));
                    let body: webdav_handler::BoxedByteStream = Box::new(body);
                    let body = hyper::Body::wrap_stream(body);
                    let response = hyper::Response::builder()
                        .status(401)
                        .header("WWW-Authenticate", "Basic realm=\"foo\"")
                        .body(body)
                        .unwrap();
                    return Box::new(futures::future::ok(response));
                },
            }
        } else {
            self.dh.clone()
        };

        // transform hyper::Request into http::Request, run handler,
        // then transform http::Response into hyper::Response.
        let (parts, body) = req.into_parts();
        let body = body.map(|item| Bytes::from(item));
        let req = http::Request::from_parts(parts, body);
        let fut = handler.handle(req)
            .and_then(|resp| {
                let (parts, body) = resp.into_parts();
                let body = hyper::Body::wrap_stream(body);
                Ok(hyper::Response::from_parts(parts, body))
            });
        Box::new(fut)
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let matches = clap_app!(webdav_lib =>
        (version: "0.1")
        (@arg PORT: -p --port +takes_value "port to listen on (4918)")
        (@arg DIR: -d --dir +takes_value "local directory to serve")
        (@arg MEMFS: -m --memfs "serve from ephemeral memory filesystem (default)")
        (@arg AUTH: -a --auth "require basic authentication")
    ).get_matches();

    let (dir, name) = match matches.value_of("DIR") {
        Some(dir) => (dir, dir),
        None => ("", "memory filesystem"),
    };
    let auth = matches.is_present("AUTH");

    let dav_server = Server::new(dir.to_string(), auth);
    let make_service = move || {
        let dav_server = dav_server.clone();
        hyper::service::service_fn(move |req| {
            dav_server.handle(req)
        })
    };

    let port = matches.value_of("PORT").unwrap_or("4918");
    let addr = "0.0.0.0:".to_string() + port;
    let addr = SocketAddr::from_str(&addr)?;
    let server = hyper::Server::try_bind(&addr)?
        .serve(make_service)
        .map_err(|e| eprintln!("server error: {}", e));

    println!("Serving {} on {}", name, port);
    hyper::rt::run(server);

    Ok(())
}

