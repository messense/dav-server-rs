
- Add a real locksystem interface, and a "memls"

- Do fake locking only for user-agents:

  - /WebDAVFS/					// Apple
  - /Microsoft Office OneNote 2013/'		// MS
  - /^Microsoft-WebDAV/				// MS

  this is the list that NextCloud uses for fake locking.
  probably (WebDAVFS|Microsoft) would do the trick.

- Add support for properties to localfs.rs on XFS. XFS has unlimited and
  scalable extended attributes. ext2/3/4 can store max 4KB. On XFS we can
  then also store creationdate in an attribute.

- Add support for setting mtime/atime

- score 100% against the litmus test :)

- move filesystem interface to pathbuf

- support for compressing responses, at least PROPFIND.
- support for compressed PUT requests?

- cache-control headers?

- add documentation
- add tests, tests ...

- port to hyper 0.11

DONE:

- add support for webdav quota reporting - RFC4331. Support plain
  linux quota and NFS rquota.
- research why UNLOCK fails with 4xx on OSX
- If: header support in conditional.rs

