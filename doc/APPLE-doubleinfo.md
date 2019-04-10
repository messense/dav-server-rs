
# APPLEDOUBLEINFO

Normally, after asking for a directory listing (using PROPFIND with Depth: 1)
the macOS Finder will send a PROPFIND request for every file in the
directory, prefixed with ".\_". Even though it just got a complete directory
listing which doesn't list those files.

An optimization the Apple iDisk service makes, is that it sometimes
synthesizes those info files ahead of time. It then lists those synthesized
files in the PROPFIND response together with the <appledoubleheader> propery,
which is the contents of the ".\_file" (if it would be present) in base64.
It appears to only do this when the appledoubleinfo data is completely
basic and is 82 bytes of size.

This prevents the webdav clients from launching an additional PROPFIND
request for every file prefixed with ".\_".

Note that you cannot add an <appledoubleheader> propery to a PROPSTAT
element of a "file" itself, that's ignored, alas. macOS only accepts
it on ".\_" files.

There is not much information about this, but an Apple engineer mentioned it in
https://lists.apple.com/archives/filesystem-dev/2009/Feb/msg00013.html

There is a default "empty"-like response for a file that I found at
https://github.com/DanRohde/webdavcgi/blob/master/lib/perl/WebDAV/Properties.pm

So, what we _could_ do (but don't, yet) to optimize the macOS webdav client,
when we reply to PROPFIND:

- for each file that does NOT have a ".\_file" present
- we synthesize a virtual response
- for a virtual file with name ".\_file
- with size: 82 bytes
- that contains:
  <DAV:prop xmlns:S="http://www.apple.com/webdav\_fs/props/">
  <S:appledoubleheader>
	AAUWBwACAAAAAAAAAAAAAAAAAAAAAAAAAAIAAAACAAAAJgAAACwAAAAJAAAAMgAAACAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==
  </S:appledoubleheader>

The contents of this base64 string are explained at
https://github.com/DanRohde/webdavcgi/blob/master/lib/perl/WebDAV/Properties.pm

... and they are:

```
  appledoubleheader: Magic(4) Version(4) Filler(16) EntryCout(2) 
  EntryDescriptor(id:4(2:resource fork),offset:4,length:4)
  EntryDescriptor(id:9 finder)... Finder Info(16+16)

  namespace: http://www.apple.com/webdav\_fs/props/
  content: MIME::Base64(pack('H\*', '00051607'. '00020000' . ( '00' x 16 ) .
   '0002'. '00000002'. '00000026' . '0000002C'.'00000009'. '00000032' . '00000020' .
   ('00' x 32) ))
```

