# HTTP PUT-with-Content-Range support.

The [mod_dav](https://httpd.apache.org/docs/2.4/mod/mod_dav.html) module of
the [Apache web server](https://httpd.apache.org/) was one of the first
implementations of [Webdav](https://tools.ietf.org/html/rfc4918). Ever since
the first released version, it has had support for partial uploads using
the Content-Range header with PUT requests.

## A sample request

```text
PUT /file.txt
Content-Length: 4
Content-Range: bytes 3-6/*

ABCD
```

This request updates 'file.txt', specifically the bytes 3-6 (inclusive) to
`ABCD`.

There is no explicit support for appending to a file, that is simply done
by writing just past the end of a file. For example, if a file has size
1000, and you want to append 4 bytes:

```text
PUT /file.txt
Content-Length: 4
Content-Range: bytes 1000-1003/*

1234
```

## Apache `mod_dav` behaviour:

- The `Content-Range` header is required, and the syntax is `bytes START-END/LENGTH`.
- END must be bigger than or equal to START.
- LENGTH is parsed by Apache mod_dav, and it must either be a valid number
  or a `*` (star), but mod_dav otherwise ignores it. Since it is not clearly
  defined what LENGTH should be, always use `*`.
- Neither the start, nor the end-byte have to be within the file's current size.
- If the start-byte is beyond the file's current length, the space in between
  will be filled with NULL bytes (`0x00`).

## Notes

- `bytes<space>`, _not_ `bytes=`.
- The `Content-Length` header is not required by the original Apache mod_dav
  implementation. The body must either have a valid Content-Length, or it must
  use the `Chunked` transfer encoding. It is *strongly encouraged* though to
  include Content-Length, so that it can be validated against the range before
  accepting the PUT request.
- If the `Content-Length` header is present, its value must be equal
  to `END - START + 1`.
  
## Status codes

### The following status codes are used:

Status code | Reason
----------- | ------
200 or 204  | When the operation was successful
400         | Invalid `Content-Range` header
416         | If there was something wrong with the bytes, such as a `Content-Length` not matching with what was sent as the start and end bytes, or an end byte being lower than the start byte.
501         | Content-Range header present, but not supported.

## RECKOGNIZING PUT-with-Content-Range support (client).

There is no official way to know if PUT-with-content-range is supported by
a webserver. For a client it's probably best to do an OPTIONS request,
and then check two things:

- the `Server` header must contain the word `Apache`
- the `DAV` header must contain `<http://apache.org/dav/propset/fs/1>`.

In that case, your are sure to talk to an Apache webserver with mod_dav enabled.

## IMPLEMENTING PUT-with-Content-Range support (server).

Don't. Implement [sabredav-partialupdate](SABREDAV-partialupdate.md).

## MORE INFORMATION.

https://blog.sphere.chronosempire.org.uk/2012/11/21/webdav-and-the-http-patch-nightmare

