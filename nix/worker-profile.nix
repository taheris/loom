profile:

let
  withoutNix = builtins.filter (
    package: (package.pname or (builtins.parseDrvName package.name).name) != "nix"
  );
in
profile
// {
  corePackages = withoutNix (profile.corePackages or [ ]);
  packages = withoutNix (profile.packages or [ ]);
}
