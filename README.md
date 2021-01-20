# webdav-handler

[![Apache-2.0 licensed](https://img.shields.io/badge/license-Apache2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0.txt)
[![crates.io](https://meritbadge.herokuapp.com/webdav-handler)](https://crates.io/crates/webdav-handler)
[![Released API docs](https://docs.rs/webdav-handler/badge.svg)](https://docs.rs/webdav-handler)

### Generic async HTTP/Webdav handler

[`Webdav`] (RFC4918) is defined as
HTTP (GET/HEAD/PUT/DELETE) plus a bunch of extension methods (PROPFIND, etc).
These extension methods are used to manage collections (like unix directories),
get information on collections (like unix `ls` or `readdir`), rename and
copy items, lock/unlock items, etc.

A `handler` is a piece of code that takes a `http::Request`, processes it in some
way, and then generates a `http::Response`. This library is a `handler` that maps
the HTTP/Webdav protocol to the filesystem. Or actually, "a" filesystem. Included
is an adapter for the local filesystem (`localfs`), and an adapter for an
in-memory filesystem (`memfs`).

So this library can be used as a handler with HTTP servers like [hyper],
[warp], [actix-web], etc. Either as a correct and complete HTTP handler for
files (GET/HEAD) or as a handler for the entire Webdav protocol. In the latter case, you can
mount it as a remote filesystem: Linux, Windows, macOS can all mount Webdav filesystems.

### Backend interfaces.

The backend interfaces are similar to the ones from the Go `x/net/webdav package`:

- the library contains a [HTTP handler][DavHandler].
- you supply a [filesystem][DavFileSystem] for backend storage, which can optionally
  implement reading/writing [DAV properties][DavProp].
- you can supply a [locksystem][DavLockSystem] that handles webdav locks.

The handler in this library works with the standard http types
from the `http` and `http_body` crates. That means that you can use it
straight away with http libraries / frameworks that also work with
those types, like hyper. Compatibility modules for [actix-web][actix-compat]
and [warp][warp-compat] are also provided.

### Implemented standards.

Currently [passes the "basic", "copymove", "props", "locks" and "http"
checks][README_litmus] of the Webdav Litmus Test testsuite. That's all of the base
[RFC4918] webdav specification.

The litmus test suite also has tests for RFC3744 "acl" and "principal",
RFC5842 "bind", and RFC3253 "versioning". Those we do not support right now.

The relevant parts of the HTTP RFCs are also implemented, such as the
preconditions (If-Match, If-None-Match, If-Modified-Since, If-Unmodified-Since,
If-Range), partial transfers (Range).

Also implemented is `partial PUT`, for which there are currently two
non-standard ways to do it: [`PUT` with the `Content-Range` header][PUT],
which is what Apache's `mod_dav` implements, and [`PATCH` with the `X-Update-Range`
header][PATCH] from `SabreDav`.

### Backends.

Included are two filesystems:

- [`LocalFs`]: serves a directory on the local filesystem
- [`MemFs`]: ephemeral in-memory filesystem. supports DAV properties.

Also included are two locksystems:

- [`MemLs`]: ephemeral in-memory locksystem.
- [`FakeLs`]: fake locksystem. just enough LOCK/UNLOCK support for macOS/Windows.

### Example.

Example server using [hyper] that serves the /tmp directory in r/w mode. You should be
able to mount this network share from Linux, macOS and Windows. [Examples][examples]
for other frameworks are also available.

```rust
use std::convert::Infallible;
use webdav_handler::{fakels::FakeLs, localfs::LocalFs, DavHandler};

#[tokio::main]
async fn main() {
    let dir = "/tmp";
    let addr = ([127, 0, 0, 1], 4918).into();

    let dav_server = DavHandler::builder()
        .filesystem(LocalFs::new(dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();

    let make_service = hyper::service::make_service_fn(move |_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move {
                    Ok::<_, Infallible>(dav_server.handle(req).await)
                }
            };
            Ok::<_, Infallible>(hyper::service::service_fn(func))
        }
    });

    println!("Serving {} on {}", dir, addr);
    let _ = hyper::Server::bind(&addr)
        .serve(make_service)
        .await
        .map_err(|e| eprintln!("server error: {}", e));
}
```
[DavHandler]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/struct.DavHandler.html
[DavFileSystem]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/fs/index.html
[DavLockSystem]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/ls/index.html
[DavProp]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/fs/struct.DavProp.html
[`WebDav`]: https://tools.ietf.org/html/rfc4918
[RFC4918]: https://tools.ietf.org/html/rfc4918
[`MemLs`]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/memls/index.html
[`MemFs`]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/memfs/index.html
[`LocalFs`]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/localfs/index.html
[`FakeLs`]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/fakels/index.html
[actix-compat]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/actix/index.html
[warp-compat]: https://docs.rs/webdav-handler/0.2.0-alpha.6/webdav_handler/warp/index.html
[README_litmus]: https://github.com/miquels/webdav-handler-rs/blob/master/README.litmus-test.md
[examples]: https://github.com/miquels/webdav-handler-rs/tree/master/examples/
[PUT]: https://github.com/miquels/webdav-handler-rs/tree/master/doc/Apache-PUT-with-Content-Range.md
[PATCH]: https://github.com/miquels/webdav-handler-rs/tree/master/doc/SABREDAV-partialupdate.md
[hyper]: https://hyper.rs/
[warp]: https://crates.io/crates/warp
[actix-web]: https://actix.rs/

### Building.

This crate uses std::future::Future and async/await, so it only works with Rust 1.39 and up.

### Testing.

```
RUST_LOG=webdav_handler=debug cargo run --example sample-litmus-server
```

This will start a server on port 4918, serving an in-memory filesystem.
For other options, run `cargo run --example sample-litmus-server -- --help`

### Copyright and License.

 * © 2018, 2019, 2020 XS4ALL Internet bv
 * © 2018, 2019, 2020 Miquel van Smoorenburg
 * [Apache License, Version 2.0](http://www.apache.org/licenses/LICENSE-2.0)
