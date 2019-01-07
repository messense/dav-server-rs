//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/<DIR>/
//
//  <DIR>   username    password    filesystem
//  public  -           -           <rootdir>/public
//  mike    mike        mike        <rootdir>/mike
//  simon   simon       simon       <rootdir>/simon 
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
};

#[derive(Clone)]
struct Server {
    dh:             DavHandler,
}

type BoxedError = Box<dyn Error + Send + Sync>;
type BoxedFuture = Box<Future<Item=hyper::Response<hyper::Body>, Error=BoxedError> + Send>;

impl Server {
    pub fn new(directory: String) -> Self {
        let dh = if directory != "" {
            let fs = localfs::LocalFs::new(directory, true);
            DavHandler::new("", fs, None)
        } else {
            let fs = memfs::MemFs::new();
            let ls = memls::MemLs::new();
            DavHandler::new("", fs, Some(ls))
        };
        Server{ dh }
    }

    fn handle(&self, req: hyper::Request<hyper::Body>) -> BoxedFuture {
        /*
        // Get request path.
        let path = match WebPath::from_str(req.path(), "") {
            Ok(path) => path,
            Err(_) => {
                return Response::builder()
                    .status(StatusCode::BadRequest)
                    .header("connection", "close")
                    .body(())
                    .unwrap();
            }
        };
        */

        // transform hyper::Request into http::Request, run handler,
        // then transform http::Response into hyper::Response.
        let (parts, body) = req.into_parts();
        let body = body.map(|item| Bytes::from(item));
        let req = http::Request::from_parts(parts, body);
        let fut = self.dh.handle(req)
            .and_then(|resp| {
                let (parts, body) = resp.into_parts();
                let body = hyper::Body::wrap_stream(body);
                Ok(hyper::Response::from_parts(parts, body))
            })
            .map_err(|e| {
                let r: Box<dyn Error + Send + Sync> = Box::new(e);
                r
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
        (@arg mem: -m --memfs "serve from ephemeral memory filesystem")
    ).get_matches();

    let (dir, name) = match matches.value_of("DIR") {
        Some(dir) => (dir, dir),
        None => ("", "memory filesystem"),
    };

    let dav_server = Server::new(dir.to_string());
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

    /*
    let server = hyper::Server::try_bind(&addr.into())?
        .tcp_nodelay(true)
        .serve(dav_server)
        .map_err(|e| eprintln!("server error: {}", e));
    */

    println!("Serving {} on {}", name, port);
    hyper::rt::run(server);

    Ok(())
}

