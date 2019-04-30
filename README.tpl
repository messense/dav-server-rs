# {{crate}}

[![Apache-2.0 licensed](https://img.shields.io/badge/license-Apache2.0-blue.svg)](https://www.apache.org/licenses/LICENSE-2.0.txt)
[![crates.io](https://meritbadge.herokuapp.com/webdav-handler)](https://crates.io/crates/webdav-handler)
[![Released API docs](https://docs.rs/webdav-handler/badge.svg)](https://docs.rs/webdav-handler)

{{readme}}

### Building.

This crate uses futures@0.3 and async/await internally, so you have to
build it with a nightly toolchain.

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

