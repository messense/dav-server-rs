use std::net::SocketAddr;

use warp::{Filter, filters::BoxedFilter, Reply};
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavHandler};

/// WarpDav maintains the state of the DavHandler.
#[derive(Clone)]
struct WarpDav {
    handler:    DavHandler,
}

impl WarpDav {
    /// Create a new WarpDav
    fn new(dir: &str) -> WarpDav {
        let h = DavHandler::builder()
            .filesystem(LocalFs::new(dir, false, false, false))
            .locksystem(FakeLs::new())
            .build_handler();
        WarpDav{ handler: h }
    }

    /// Return a reply-filter for DAV requests.
    fn dav(&self) -> BoxedFilter<(impl Reply,)> {

        use http::Response;
        use http::header::HeaderMap;
        use http::uri::Uri;
        use warp::path::{FullPath, Tail};

        let this = self.clone();

        warp::method()
            .and(warp::path::full())
            .and(warp::path::tail())
            .and(warp::header::headers_cloned())
            .and(warp::body::stream())
            .map(|method, path_full: FullPath, path_tail: Tail, headers: HeaderMap, body| {

                // Get the prefix.
                let path_str = path_full.as_str();
                let path_len = path_str.len();
                let tail_len = path_tail.as_str().len();
                let prefix = path_str[..path_len - tail_len].to_string();

                // rebuild an http::Request struct.
                let uri = path_str.parse::<Uri>().unwrap();
                let mut builder = http::Request::builder()
                    .method(method)
                    .uri(uri);
                for (k, v) in headers.iter() {
                    builder = builder.header(k, v);
                }

                // Pass request and prefix to the next closure.
                (builder.body(body).unwrap(), prefix)
            })
            .untuple_one()
            .and_then(move |request, prefix| {
                let this = this.clone();
                async move {
                    // Run a handler with the current path prefix.
                    let config = DavHandler::builder().strip_prefix(prefix);
                    let result = this.handler.handle_stream_with(config, request).await;

                    // Need to remap the http_body::Body to a hyper::Body.
                    result.map(|resp| {
                        let (parts, body) = resp.into_parts();
                        Response::from_parts(parts, hyper::Body::wrap_stream(body))
                    })
                    // The DavHandler never returns an error, but we need to
                    // put something here to keep the compiler happy.
                    .map_err(|_| warp::reject::reject())
                }
            })
            .boxed()
    }
}

#[tokio::main(threaded_scheduler)]
async fn main() {
    env_logger::init();
    let dir = "/tmp";
    let addr: SocketAddr = ([127, 0, 0, 1], 4918).into();

    println!("Serving {} on {}", dir, addr);
    let warpdav = WarpDav::new(dir);
    warp::serve(warpdav.dav()).run(addr).await;
}
