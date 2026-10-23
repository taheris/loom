# A native derivation output retains realization duties without an acceptance annotation.
# Preserve an existing constructor's provenance; transitive input origins are joined
# from the actual derivation graph, not from a second file registry.
value:
let
  checked = import ./verification.nix;
in
checked.check {
  resources = [ ];
  evaluation = import ./native-context.nix;
  build = _: value;
}
