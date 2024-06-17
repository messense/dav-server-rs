//! Adapter for the `warp` HTTP server framework.
//!
//! The filters in this module will always succeed and never
//! return an error. For example, if a file is not found, the
//! filter will return a 404 reply, and not an internal
//! rejection.
//!
use std::convert::Infallible;
#[cfg(any(docsrs, feature = "localfs"))]
use std::path::Path;

use warp::{
    filters::BoxedFilter,
    http::{HeaderMap, Method},
    Filter, Reply,
};

use crate::{body::Body, DavHandler};
#[cfg(any(docsrs, feature = "localfs"))]
use crate::{fakels::FakeLs, localfs::LocalFs};

/// Reply-filter that runs a DavHandler.
///
/// Just pass in a pre-configured DavHandler. If a prefix was not
/// configured, it will be the request path up to this point.
pub fn dav_handler(handler: DavHandler) -> BoxedFilter<(impl Reply,)> {
    use http::uri::Uri;
    use warp::path::{FullPath, Tail};

    warp::method()
        .and(warp::path::full())
        .and(warp::path::tail())
        .and(warp::header::headers_cloned())
        .and(warp::body::stream())
        .and_then(
            move |method: Method,
                  path_full: FullPath,
                  path_tail: Tail,
                  headers: HeaderMap,
                  body| {
                let handler = handler.clone();

                async move {
                    // rebuild an http::Request struct.
                    let path_str = path_full.as_str();
                    let uri = path_str.parse::<Uri>().unwrap();
                    let mut builder = http::Request::builder().method(method.as_ref()).uri(uri);
                    for (k, v) in headers.iter() {
                        builder = builder.header(k.as_str(), v.as_ref());
                    }
                    let request = builder.body(body).unwrap();

                    let response = if handler.config.prefix.is_some() {
                        // Run a handler with the configured path prefix.
                        handler.handle_stream(request).await
                    } else {
                        // Run a handler with the current path prefix.
                        let path_len = path_str.len();
                        let tail_len = path_tail.as_str().len();
                        let prefix = path_str[..path_len - tail_len].to_string();
                        let config = DavHandler::builder().strip_prefix(prefix);
                        handler.handle_stream_with(config, request).await
                    };

                    // Need to remap the http_body::Body to a hyper::Body.
                    let response = warp_response(response).unwrap();
                    Ok::<_, Infallible>(response)
                }
            },
        )
        .boxed()
}

/// Creates a Filter that serves files and directories at the
/// base path joined with the remainder of the request path,
/// like `warp::filters::fs::dir`.
///
/// The behaviour for serving a directory depends on the flags:
///
/// - `index_html`: if an `index.html` file is found, serve it.
/// - `auto_index_over_get`: Create a directory index page when accessing over HTTP `GET` (but NOT
///   affecting WebDAV `PROPFIND` method currently). In the current implementation, this only
///   affects HTTP `GET` method (commonly used for listing the directories when accessing through a
///   `http://` or `https://` URL for a directory in a browser), but NOT WebDAV listing of a
///   directory (HTTP `PROPFIND`). BEWARE: The name and behaviour of this parameter variable may
///   change, and later it may control WebDAV `PROPFIND`, too (but not as of now).
///   
///   In release mode, if `auto_index_over_get` is `true`, then this executes as described above
///   (currently affecting only HTTP `GET`), but beware of this current behaviour.
///   
///   In debug mode, if `auto_index_over_get` is `false`, this _panics_. That is so that it alerts
///   the developers to this current limitation, so they don't accidentally expect
///   `auto_index_over_get` to control WebDAV.
/// - no flags set: 404.
#[cfg(any(docsrs, feature = "localfs"))]
pub fn dav_dir(
    base: impl AsRef<Path>,
    index_html: bool,
    auto_index_over_get: bool,
) -> BoxedFilter<(impl Reply,)> {
    debug_assert!(
        auto_index_over_get,
        "See documentation of dav_server::warp::dav_dir(...)."
    );
    let mut builder = DavHandler::builder()
        .filesystem(LocalFs::new(base, false, false, false))
        .locksystem(FakeLs::new())
        .autoindex(auto_index_over_get);
    if index_html {
        builder = builder.indexfile("index.html".to_string())
    }
    let handler = builder.build_handler();
    dav_handler(handler)
}

/// Creates a Filter that serves a single file, ignoring the request path,
/// like `warp::filters::fs::file`.
#[cfg(any(docsrs, feature = "localfs"))]
pub fn dav_file(file: impl AsRef<Path>) -> BoxedFilter<(impl Reply,)> {
    let handler = DavHandler::builder()
        .filesystem(LocalFs::new_file(file, false))
        .locksystem(FakeLs::new())
        .build_handler();
    dav_handler(handler)
}

/// Adapts the response to the `warp` versions of `hyper` and `http` while `warp` remains on old versions.
/// https://github.com/seanmonstar/warp/issues/1088
fn warp_response(
    response: http::Response<Body>,
) -> Result<warp::http::Response<warp::hyper::Body>, warp::http::Error> {
    let (parts, body) = response.into_parts();
    // Leave response extensions empty.
    let mut response = warp::http::Response::builder()
        .version(warp_http_version(parts.version))
        .status(parts.status.as_u16());
    // Ignore headers without the name.
    let headers = parts.headers.into_iter().filter_map(|(k, v)| Some((k?, v)));
    for (k, v) in headers {
        response = response.header(k.as_str(), v.as_ref());
    }
    response.body(warp::hyper::Body::wrap_stream(body))
}

/// Adapts HTTP version to the `warp` version of `http` crate while `warp` remains on old version.
/// https://github.com/seanmonstar/warp/issues/1088
fn warp_http_version(v: http::Version) -> warp::http::Version {
    match v {
        http::Version::HTTP_3 => warp::http::Version::HTTP_3,
        http::Version::HTTP_2 => warp::http::Version::HTTP_2,
        http::Version::HTTP_11 => warp::http::Version::HTTP_11,
        http::Version::HTTP_10 => warp::http::Version::HTTP_10,
        http::Version::HTTP_09 => warp::http::Version::HTTP_09,
        v => unreachable!("unexpected HTTP version {:?}", v),
    }
}
