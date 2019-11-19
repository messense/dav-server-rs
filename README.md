# webdav-handler

[![Apache-2.0 licensed](https://img.shields.io/badge/license-Apache2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0.txt)
[![crates.io](https://meritbadge.herokuapp.com/webdav-handler)](https://crates.io/crates/webdav-handler)
[![Released API docs](https://docs.rs/webdav-handler/badge.svg)](https://docs.rs/webdav-handler)

### Generic async HTTP/WEBDAV handler

[`Webdav`] ([RFC4918]) is HTTP (GET/HEAD/PUT/DELETE) plus a bunch of extra methods.

This crate implements a futures/stream based webdav handler for Rust, using
the types from the `http` crate. It comes complete with an async filesystem
backend, so it can be used as a WEBDAV filesystem server, or just as a
feature-complete HTTP server.

### Interface.

It has an interface similar to the Go x/net/webdav package:

- the library contains an [HTTP handler][DavHandler]
- you supply a [filesystem][DavFileSystem] for backend storage, which can optionally
  implement reading/writing [DAV properties][DavProp].
- you can supply a [locksystem][DavLockSystem] that handles the webdav locks

With some glue code, this handler can be used from HTTP server
libraries/frameworks such as [hyper].
(See [examples/hyper.rs][hyper_example]).

### Implemented standards.

Currently [passes the "basic", "copymove", "props", "locks" and "http"
checks][README_litmus] of the Webdav Litmus Test testsuite. That's all of the base
[RFC4918] webdav specification.

The litmus test suite also has tests for RFC3744 "acl" and "principal",
RFC5842 "bind", and RFC3253 "versioning". Those we do not support right now.

The relevant parts of the HTTP RFCs are also implemented, such as the
preconditions (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since,
If-Range), partial transfers (Range).

Also implemented is `partial PUT`, for which there are currently [two
non-standard ways][PartialPut] to do it: [`PUT` with the `Content-Range` header][PUT],
which is what Apache's `mod_dav` implements, and [`PATCH` with the `X-Update-Range`
header][PATCH] from `SabreDav`.

### Backends.

Included are two filesystems:

- [`LocalFs`]: serves a directory on the local filesystem
- [`MemFs`]: ephemeral in-memory filesystem. supports DAV properties.

Also included are two locksystems:

- [`MemLs`]: ephemeral in-memory locksystem.
- [`FakeLs`]: fake locksystem. just enough LOCK/UNLOCK support for OSX/Windows.

### Example.

Example server that serves the /tmp directory in r/w mode. You should be
able to mount this network share from Linux, OSX and Windows.

```rust
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavHandler};

#[tokio::main]
async fn main() {
    let dir = "/tmp";
    let addr = ([127, 0, 0, 1], 4918).into();

    let dav_server = DavHandler::new(None, LocalFs::new(dir, false, false, false), Some(FakeLs::new()));
    let make_service = hyper::service::make_service_fn(move |_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move {
                    dav_server.handle(req).await
                }
            };
            Ok::<_, hyper::Error>(hyper::service::service_fn(func))
        }
    });

    println!("Serving {} on {}", dir, addr);
    let _ = hyper::Server::bind(&addr)
        .serve(make_service)
        .await
        .map_err(|e| eprintln!("server error: {}", e));
}
```
[DavHandler]: https://docs.rs/webdav-handler/0.2.0/struct.DavHandler.html
[DavFileSystem]: https://docs.rs/webdav-handler/0.2.0/fs/index.html
[DavLockSystem]: https://docs.rs/webdav-handler/0.2.0/ls/index.html
[DavProp]: https://docs.rs/webdav-handler/0.2.0/fs/struct.DavProp.html
[`WebDav`]: https://tools.ietf.org/html/rfc4918
[RFC4918]: https://tools.ietf.org/html/rfc4918
[`MemLs`]: https://docs.rs/webdav-handler/0.2.0/memls/index.html
[`MemFs`]: https://docs.rs/webdav-handler/0.2.0/memfs/index.html
[`LocalFs`]: https://docs.rs/webdav-handler/0.2.0/localfs/index.html
[`FakeLs`]: https://docs.rs/webdav-handler/0.2.0/fakels/index.html
[README_litmus]: https://github.com/miquels/webdav-handler-rs/blob/master/README.litmus-test.md
[hyper_example]: https://github.com/miquels/webdav-handler-rs/blob/master/examples/hyper.rs
[PartialPut]: https://blog.sphere.chronosempire.org.uk/2012/11/21/webdav-and-the-http-patch-nightmare
[PUT]: https://blog.sphere.chronosempire.org.uk/2012/11/21/webdav-and-the-http-patch-nightmare
[PATCH]: https://github.com/miquels/webdavfs/blob/master/SABREDAV-partialupdate.md
[hyper]: https://hyper.rs/


### Building.

This crate uses std::future::Future and async/await, so it only works with Rust 1.39 and up.

### Testing.

```
RUST_LOG=webdav_handler=debug cargo run --example sample-litmus-server
```

This will start a server on port 4918, serving an in-memory filesystem.
For other options, run `cargo run --example sample-litmus-server -- --help`

### Copyright and License.

 * © 2018, 2019 XS4ALL Internet bv
 * © 2018, 2019 Miquel van Smoorenburg
 * [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
