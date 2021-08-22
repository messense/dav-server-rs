//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/
//

use std::convert::Infallible;
use std::error::Error;
use std::net::SocketAddr;
use std::str::FromStr;

#[macro_use]
extern crate clap;

use env_logger;
use futures_util::future::TryFutureExt;
use hyper;

use headers::{authorization::Basic, Authorization, HeaderMapExt};

use webdav_handler::{body::Body, fakels, localfs, memfs, memls, DavConfig, DavHandler};

#[derive(Clone)]
struct Server {
    dh:   DavHandler,
    auth: bool,
}

impl Server {
    pub fn new(directory: String, memls: bool, fakels: bool, auth: bool) -> Self {
        let mut config = DavHandler::builder();
        if directory != "" {
            config = config.filesystem(localfs::LocalFs::new(directory, true, true, true));
        } else {
            config = config.filesystem(memfs::MemFs::new());
        };
        if fakels {
            config = config.locksystem(fakels::FakeLs::new());
        }
        if memls {
            config = config.locksystem(memls::MemLs::new());
        }

        Server {
            dh: config.build_handler(),
            auth,
        }
    }

    async fn handle(&self, req: hyper::Request<hyper::Body>) -> Result<hyper::Response<Body>, Infallible> {
        let user = if self.auth {
            // we want the client to authenticate.
            match req.headers().typed_get::<Authorization<Basic>>() {
                Some(Authorization(basic)) => Some(basic.username().to_string()),
                None => {
                    // return a 401 reply.
                    let response = hyper::Response::builder()
                        .status(401)
                        .header("WWW-Authenticate", "Basic realm=\"foo\"")
                        .body(Body::from("please auth".to_string()))
                        .unwrap();
                    return Ok(response);
                },
            }
        } else {
            None
        };

        if let Some(user) = user {
            let config = DavConfig::new().principal(user);
            Ok(self.dh.handle_with(config, req).await)
        } else {
            Ok(self.dh.handle(req).await)
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
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
                async move { dav_server.clone().handle(req).await }
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
    let _ = server.await;
    Ok(())
}
