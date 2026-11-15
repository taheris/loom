#!/usr/bin/env bash
set -euo pipefail

target="$1"
shift
# Dependency artifacts remain shared. Workspace outputs can be overwritten at
# the same Cargo hash when source or tracked build-script environment changes.
for directory in "$target"/*/deps "$target"/*/*/deps; do
  [[ -d "$directory" ]] || continue
  for name in "$@"; do
    for artifact in "$directory/lib$name".{rlib,rmeta,so,dylib} "$directory/lib$name-"*.{rlib,rmeta,so,dylib}; do
      [[ -L "$artifact" ]] || continue
      cp --dereference --preserve=timestamps -- "$artifact" "$artifact.writable"
      chmod u+w -- "$artifact.writable"
      mv -f -- "$artifact.writable" "$artifact"
    done
  done
done
