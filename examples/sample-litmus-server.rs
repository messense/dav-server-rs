//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/
//

use std::error::Error;
use std::io;
use std::net::SocketAddr;
use std::str::FromStr;

#[macro_use]
extern crate clap;

use env_logger;
use futures::future::TryFutureExt;
use hyper;

use headers::{Authorization, authorization::Basic, HeaderMapExt};

use webdav_handler::{
    localfs,
    ls::DavLockSystem,
    memfs, memls, fakels,
    body::Body,
    DavConfig, DavHandler,
};

#[derive(Clone)]
struct Server {
    dh:   DavHandler,
    auth: bool,
}

impl Server {
    pub fn new(directory: String, memls: bool, fakels: bool, auth: bool) -> Self {
        let ls: Option<Box<dyn DavLockSystem>> = if fakels {
            Some(fakels::FakeLs::new())
        } else if memls {
            Some(memls::MemLs::new())
        } else {
            None
        };
        let dh = if directory != "" {
            let fs = localfs::LocalFs::new(directory, true, true, true);
            DavHandler::new(None, fs, ls)
        } else {
            let fs = memfs::MemFs::new();
            DavHandler::new(None, fs, ls)
        };
        Server { dh, auth }
    }

    async fn handle(&self, req: hyper::Request<hyper::Body>) -> io::Result<hyper::Response<Body>> {

        let user = if self.auth {
            // we want the client to authenticate.
            match req.headers().typed_get::<Authorization<Basic>>() {
                Some(Authorization(basic)) => Some(basic.username().to_string()),
                None => {
                    // return a 401 reply.
                    let response = hyper::Response::builder()
                        .status(401)
                        .header("WWW-Authenticate", "Basic realm=\"foo\"")
                        .body(Body::from("please auth"))
                        .unwrap();
                    return Ok(response);
                },
            }
        } else {
            None
        };

        let config = DavConfig {
            principal: user,
            ..DavConfig::default()
        };

        self.dh.handle_with(config, req).await
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let matches = clap_app!(webdav_lib =>
        (version: "0.1")
        (@arg PORT: -p --port +takes_value "port to listen on (4918)")
        (@arg DIR: -d --dir +takes_value "local directory to serve")
        (@arg MEMFS: -m --memfs "serve from ephemeral memory filesystem (default)")
        (@arg MEMLS: -l --memls "use ephemeral memory locksystem (default with --memfs)")
        (@arg FAKELS: -f --fakels "use fake memory locksystem (default with --memfs)")
        (@arg AUTH: -a --auth "require basic authentication")
    )
    .get_matches();

    let (dir, name) = match matches.value_of("DIR") {
        Some(dir) => (dir, dir),
        None => ("", "memory filesystem"),
    };
    let auth = matches.is_present("AUTH");
    let memls = matches.is_present("MEMFS") || matches.is_present("MEMLS");
    let fakels = matches.is_present("FAKELS");

    let dav_server = Server::new(dir.to_string(), memls, fakels, auth);
    let make_service = hyper::service::make_service_fn(|_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move {
                    dav_server.clone().handle(req).await
                }
            };
            Ok::<_, hyper::Error>(hyper::service::service_fn(func))
        }
    });

    let port = matches.value_of("PORT").unwrap_or("4918");
    let addr = "0.0.0.0:".to_string() + port;
    let addr = SocketAddr::from_str(&addr)?;
    let server = hyper::Server::try_bind(&addr)?
        .serve(make_service)
        .map_err(|e| eprintln!("server error: {}", e));

    println!("Serving {} on {}", name, port);
    let runtime = tokio::runtime::Runtime::new()?;
    let _ = runtime.block_on(server);

    Ok(())
}
