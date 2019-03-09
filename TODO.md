
# TODO list

## Protocol compliance

### Apply all headers

The RFC says that for COPY/MOVE/DELETE with Depth: Infinity all headers
must be applied to all resources. For example, in RFC4918 9.6.1:

```
Any headers included with DELETE MUST be applied in processing every resource to be deleted.
```

Currently we do not do this- we do apply the If-Match, If-None-Match, If-Modified-Since,
If-Unmodified-Since, and If headers to the request url, but not recursively.

### In MOVE/DELETE test locks seperately per resource

Right now we check if we hold the locks (if any) for the request url, and paths
below it for Depth: Infinity requests. If we don't, the entire request fails. We
should really check that for every resource to be MOVEd/DELETEd seperately
and only fail those resources.

This does mean that we cannot MOVE a collection by doing a simple rename, we must
do it resource-per-resource, like COPY.

## Race conditions

During long-running requests like MOVE/COPY/DELETE we should really LOCK the resource
so that no other request can race us.

Actually, check if this is true. Isn't the webdav client responsible for this?

Anyway:

- if the resource is locked exclusively and we hold the lock- great, nothing to do
- otherwise:
- lock the request URL exclusively (unless already locked exclusively), Depth: infinite,
  _without checking if any other locks already exist_. This is a temporary lock.
- now check if we actually can lock the request URL and paths below
- if not, unlock, error
- go ahead and do the work
- unlock

The temporary lock should probably have a timeout of say 10 seconds, where we
refresh it every 5 seconds or so, so that a stale lock doesn't hang around
too long if something goes catastrophically wrong. Might only happen when
the lock database is seperate from the webdav server.

## Improvements:

- Do fake locking only for user-agents:

  - /WebDAVFS/					// Apple
  - /Microsoft Office OneNote 2013/'		// MS
  - /^Microsoft-WebDAV/				// MS

  this is the list that NextCloud uses for fake locking.
  probably (WebDAVFS|Microsoft) would do the trick.

- API: perhaps move filesystem interface to Path/PathBuf or similar and hide WebPath

- add documentation
- add tests, tests ...

## Project ideas:

- Add support for properties to localfs.rs on XFS. XFS has unlimited and
  scalable extended attributes. ext2/3/4 can store max 4KB. On XFS we can
  then also store creationdate in an attribute.

- Add support for setting mtime/atime

- return windows "hidden" attribute for windows clients if filename starts with "."

- implement [RFC4437 Webdav Redirectref](https://tools.ietf.org/html/rfc4437) -- basically support for symbolic links

- implement [RFC3744 Webdac ACL](https://tools.ietf.org/html/rfc3744)

## Things I thought of but aren't going to work:

### Compression

- support for compressing responses, at least PROPFIND.
- support for compressed PUT requests

Nice, but no webdav client that I know of uses compression.

