{ lockfile }:

let
  inherit (builtins)
    filter
    fromTOML
    head
    isString
    length
    readFile
    ;
  packages = (fromTOML (readFile lockfile)).package;
  matches = filter (package: (package.name or null) == "nextest-filtering") packages;
  package =
    if length matches == 1 then
      head matches
    else
      throw "Cargo.lock must contain exactly one nextest-filtering package";
in
if
  package ? version
  && package ? checksum
  && isString package.version
  && package.version != ""
  && isString package.checksum
  && builtins.match "[0-9a-f]{64}" package.checksum != null
then
  "${package.version} ${package.checksum}"
else
  throw "Cargo.lock nextest-filtering package must have a version and registry checksum"
