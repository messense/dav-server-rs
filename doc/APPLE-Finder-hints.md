# APPLE-FINDER-HINTS

The Apple Finder (and other subsystems) seem to probe for a few
files at the root of the filesystems to get a hint about the
behaviour they should show processing this filesystem.

## FILES

- `.metadata_never_index`
  prevent the system from indexing all of the data
- `.ql_disablethumbnails`
  prevent the system from downloading all files that look like an
  image or a video to create a thumbnail
- `.ql_disablecache`
  not really sure bit it sounds useful

The `.ql_` files are configuration for the "QuickLook" functionality
of the Finder.

The ".metadata\_never\_index" file appears to be a hint for the
Spotlight indexing system.

Additionally, the Finder probes for a `.localized` file in every
directory it encounters.

## OPTIMIZATIONS

For a macOS client we return the metadata for a zero-sized file if it
does a PROPSTAT of `/.metadata_never_index` or `/.ql_disablethumbnails`.

We always return a 404 Not Found for a PROPSTAT of any `.localized` file.

Furthermore, we disallow moving, removing etc of those files. The files
do not show up in a PROPSTAT of the rootdirectory.

