{ pkgs, normalizer }:
pkgs.runCommand "normalize-nextest-archive"
  {
    nativeBuildInputs = [
      (pkgs.lib.getBin pkgs.bash)
      pkgs.gnutar
      pkgs.zstd
      pkgs.diffutils
      pkgs.jq
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
    for epoch in 11 22; do
      root="/temporary/build-$epoch/target"
      jq -n --arg root "$root" '{
        "rust-build-meta": {"target-directory": $root},
        "rust-binaries": {"fixture": {"binary-path": ($root + "/release/deps/payload")}},
        "source": "/nix/store/immutable-source",
        "unrelated": ($root + "-other")
      }' > original/target/nextest/binaries-metadata.json
      jq -n --arg root "$root" '{target_directory: $root, build_directory: $root}' \
        > original/target/nextest/cargo-metadata.json
      tar --mtime="@$epoch" -C original -cf - target | zstd -q -c > "paths-$epoch.tar.zst"
      bash ${normalizer} "paths-$epoch.tar.zst" "canonical-$epoch.tar.zst"
      mkdir "paths-$epoch"
      zstd -dc "canonical-$epoch.tar.zst" | tar -xf - -C "paths-$epoch"
      jq -e --arg root "$root" '
        .["rust-build-meta"]["target-directory"] == "/target" and
        .["rust-binaries"].fixture["binary-path"] == "/target/release/deps/payload" and
        .source == "/nix/store/immutable-source" and .unrelated == ($root + "-other")
      ' "paths-$epoch/target/nextest/binaries-metadata.json"
      cmp original/target/release/deps/payload "paths-$epoch/target/release/deps/payload"
      jq 'del(.unrelated)' "paths-$epoch/target/nextest/binaries-metadata.json" > "metadata-$epoch.json"
    done
    cmp metadata-11.json metadata-22.json
    cmp paths-11/target/nextest/cargo-metadata.json paths-22/target/nextest/cargo-metadata.json
    jq '.["rust-build-meta"]["target-directory"] = "/"' \
      original/target/nextest/binaries-metadata.json > invalid-metadata.json
    mv invalid-metadata.json original/target/nextest/binaries-metadata.json
    tar -C original -cf - target | zstd -q -c > invalid-root.tar.zst
    if bash ${normalizer} invalid-root.tar.zst invalid-root-output.tar.zst; then
      echo 'invalid target root was accepted' >&2
      exit 1
    fi
    printf 'not a tar archive\n' | zstd -q -c > invalid.tar.zst
    if bash ${normalizer} invalid.tar.zst invalid-output.tar.zst; then
      echo 'malformed archive was accepted' >&2
      exit 1
    fi
    touch "$out"
  ''
