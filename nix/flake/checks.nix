_:

{
  perSystem =
    { loom, ... }:
    {
      checks = {
        inherit (loom) bin clippy nextest;
      };
    };
}
