//
//  Sample application.
//
//  Listens on localhost:4918, plain http, no ssl.
//  Connect to http://localhost:4918/
//

use std::{convert::Infallible, error::Error, net::SocketAddr};

use clap::Parser;
use headers::{authorization::Basic, Authorization, HeaderMapExt};
use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use dav_server::{body::Body, fakels, localfs, memfs, memls, DavConfig, DavHandler};

#[derive(Clone)]
struct Server {
    dh: DavHandler,
    auth: bool,
}

impl Server {
    pub fn new(directory: String, memls: bool, fakels: bool, auth: bool) -> Self {
        let mut config = DavHandler::builder();
        if !directory.is_empty() {
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

    async fn handle(
        &self,
        req: hyper::Request<hyper::body::Incoming>,
    ) -> Result<hyper::Response<Body>, Infallible> {
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
                }
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

#[derive(Debug, clap::Parser)]
#[command(about, version)]
struct Cli {
    /// port to listen on
    #[arg(short = 'p', long, default_value = "4918")]
    port: u16,
    /// local directory to serve
    #[arg(short = 'd', long)]
    dir: Option<String>,
    /// serve from ephemeral memory filesystem
    #[arg(short = 'm', long)]
    memfs: bool,
    /// use ephemeral memory locksystem
    #[arg(short = 'l', long)]
    memls: bool,
    /// use fake memory locksystem
    #[arg(short = 'f', long)]
    fakels: bool,
    /// require basic authentication
    #[arg(short = 'a', long)]
    auth: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let args = Cli::parse();

    let (dir, name) = match args.dir.as_ref() {
        Some(dir) => (dir.as_str(), dir.as_str()),
        None => ("", "memory filesystem"),
    };
    let auth = args.auth;
    let memls = args.memfs || args.memls;
    let fakels = args.fakels;

    let dav_server = Server::new(dir.to_string(), memls, fakels, auth);

    let port = args.port;
    let addr: SocketAddr = ([0, 0, 0, 0], port).into();
    let listener = TcpListener::bind(addr).await?;

    println!("Serving {} on {}", name, port);

    // We start a loop to continuously accept incoming connections
    loop {
        let (stream, _) = listener.accept().await?;
        let dav_server = dav_server.clone();

        // Use an adapter to access something implementing `tokio::io` traits as if they implement
        // `hyper::rt` IO traits.
        let io = TokioIo::new(stream);

        // Spawn a tokio task to serve multiple connections concurrently
        tokio::task::spawn(async move {
            // Finally, we bind the incoming connection to our `hello` service
            if let Err(err) = http1::Builder::new()
                // `service_fn` converts our function in a `Service`
                .serve_connection(
                    io,
                    service_fn({
                        move |req| {
                            let dav_server = dav_server.clone();
                            async move { dav_server.clone().handle(req).await }
                        }
                    }),
                )
                .await
            {
                eprintln!("Error serving connection: {:?}", err);
            }
        });
    }
}
