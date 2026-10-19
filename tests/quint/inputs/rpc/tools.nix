{ pkgs }:
let
  upstream =
    assert pkgs.quint.version == "0.33.0";
    pkgs.quint;
  solver = "share/quint/apalache-dist-0.62.1/apalache";
in
upstream.overrideAttrs (old: {
  nativeBuildInputs = (old.nativeBuildInputs or [ ]) ++ [ pkgs.jdk21_headless ];
  postInstall = (old.postInstall or "") + ''
    mkdir -p "$out/share/loom-apalache"
    cp ${./LoomApalache.java} LoomApalache.java
    javac -cp "$out/${solver}/lib/apalache.jar" \
      -d "$out/share/loom-apalache" LoomApalache.java
    rm "$out/${solver}/bin/apalache-mc"
    makeWrapper ${pkgs.coreutils}/bin/timeout "$out/${solver}/bin/apalache-mc" \
      --add-flags "--kill-after=5s 300s ${pkgs.jdk21_headless}/bin/java" \
      --add-flags "-Xms64m -Xmx2048m -XX:+ExitOnOutOfMemoryError" \
      --add-flags "--enable-native-access=ALL-UNNAMED" \
      --add-flags "-cp $out/share/loom-apalache:$out/${solver}/lib/apalache.jar LoomApalache" \
      --unset JAVA_TOOL_OPTIONS --unset JDK_JAVA_OPTIONS --unset _JAVA_OPTIONS
  '';
})
