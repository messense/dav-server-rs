
# webdav-lib

A webdav library for Rust. Uses a similar interface as the
Go webdav package:

- the library contains an HTTP handler (for Hyper 0.10.x at the moment)
- you supply a "filesystem", "locksystem" and "propsystem" that are
  used as backend storage.
- a "filesystem" that just presents the local filesystem is included

There is as of yet no "locksystem" or "propsystem". All live properties
are supported, though.

We do _fake_ a bit of "dead properties" and "locking" support, *just*
enough so that Windows and OSX can mount and use a webdav folder.

# testing

```
cd src
RUST_LOG=webdav_lib=debug cargo run
```

This will start a server on port 4918.
For login information, see src/bin/main.rs.


