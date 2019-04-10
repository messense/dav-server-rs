# APPLE-FINDER-HINTS

The Apple Finder (and other subsystems) seem to probe for a few
files at the root of the filesystems to get a hint about the
behaviour they should show processing this filesystem.

It also looks for files with extra localization information in
every directory, and for resource fork data (the `._` files).

## FILES

- `.metadata_never_index`
  prevents the system from indexing all of the data
- `.ql_disablethumbnails`
  prevent the system from downloading all files that look like an
  image or a video to create a thumbnail
- `.ql_disablecache`
  not really sure but it sounds useful

The `.ql_` files are configuration for the "QuickLook" functionality
of the Finder.

The `.metadata_never_index` file appears to be a hint for the
Spotlight indexing system.

Additionally, the Finder probes for a `.localized` file in every
directory it encounters, and it does a PROPSTAT for every file
in the directory prefixed with `._`.

## OPTIMIZATIONS

For a macOS client we return the metadata for a zero-sized file if it
does a PROPSTAT of `/.metadata_never_index` or `/.ql_disablethumbnails`.

We always return a 404 Not Found for a PROPSTAT of any `.localized` file.

Furthermore, we disallow moving, removing etc of those files. The files
do not show up in a PROPSTAT of the rootdirectory.

If a PROPFIND with `Depth: 1` is done on a directory, we add the
directory pathname to an LRU cache, and the pathname of each file of
which the name starts with `._`. Since we then know which `._` files
exist, it is easy to return a fast 404 for PROPSTAT request for `._`
files that do not exist. The cache is kept consistent by checking
the timestamp on the parent directory, and a timeout.

