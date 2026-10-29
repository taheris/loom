{ pkgs, normalizer }:
pkgs.runCommand "normalize-nextest-archive"
  {
    nativeBuildInputs = [
      pkgs.bash
      pkgs.gnutar
      pkgs.zstd
      pkgs.diffutils
    ];
  }
  ''
    mkdir -p original/target/nextest original/target/release/deps
    printf '{"fixture":"unchanged metadata"}\n' > original/target/nextest/binaries-metadata.json
    printf '{"fixture":"unchanged Cargo data"}\n' > original/target/nextest/cargo-metadata.json
    printf 'opaque\000binary\377payload\n' > original/target/release/deps/payload
    chmod 0755 original/target/release/deps/payload
    ln -s payload original/target/release/deps/alias
    for epoch in 11 22; do
      tar --mtime="@$epoch" -C original -cf - target | zstd -q -c > "raw-$epoch.tar.zst"
      bash ${normalizer} "raw-$epoch.tar.zst" "normal-$epoch.tar.zst"
    done
    if cmp raw-11.tar.zst raw-22.tar.zst; then
      echo 'negative control did not vary archive timestamps' >&2
      exit 1
    fi
    cmp normal-11.tar.zst normal-22.tar.zst
    mkdir restored
    zstd -dc normal-11.tar.zst | tar -xf - -C restored
    diff -r original restored
    [[ "$(stat -c %a restored/target/release/deps/payload)" == 755 ]]
    [[ "$(readlink restored/target/release/deps/alias)" == payload ]]
    printf 'not a tar archive\n' | zstd -q -c > invalid.tar.zst
    if bash ${normalizer} invalid.tar.zst invalid-output.tar.zst; then
      echo 'malformed archive was accepted' >&2
      exit 1
    fi
    touch "$out"
  ''
