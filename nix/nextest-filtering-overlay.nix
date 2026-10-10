# Crane overlay: patch the registry crate while retaining its Cargo.lock identity.
_final: prev: {
  downloadCargoPackage =
    args:
    let
      crate = prev.downloadCargoPackage args;
    in
    if args.name == "nextest-filtering" then
      crate.overrideAttrs (old: {
        patches = (old.patches or [ ]) ++ [ ./patches/nextest-filtering-parse.patch ];
      })
    else
      crate;
}
