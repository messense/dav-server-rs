
# {{crate}}

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

