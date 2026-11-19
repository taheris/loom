#!/usr/bin/env bash
set -euo pipefail
owned=${1:?owned Skopeo wrapper required}
raw=${2:?upstream Skopeo required}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
oci="$work/source"
mkdir -p "$oci/blobs/sha256"
printf '%s\n' '{"imageLayoutVersion":"1.0.0"}' > "$oci/oci-layout"
printf '%s\n' '{"architecture":"arm64","os":"linux","rootfs":{"type":"layers","diff_ids":[]},"config":{}}' > "$work/config"
blob() {
  local source=$1 digest
  digest=$(sha256sum "$source" | cut -d ' ' -f 1)
  cp "$source" "$oci/blobs/sha256/$digest"
  printf '%s' "$digest"
}
config=$(blob "$work/config")
jq -cn --arg digest "sha256:$config" --argjson size "$(wc -c < "$work/config")" \
  '{schemaVersion:2,mediaType:"application/vnd.oci.image.manifest.v1+json",config:{mediaType:"application/vnd.oci.image.config.v1+json",digest:$digest,size:$size},layers:[]}' > "$work/manifest"
manifest=$(blob "$work/manifest")
jq -cn --arg digest "sha256:$manifest" --argjson size "$(wc -c < "$work/manifest")" \
  '{schemaVersion:2,manifests:[{mediaType:"application/vnd.oci.image.manifest.v1+json",digest:$digest,size:$size,annotations:{"org.opencontainers.image.ref.name":"latest"}}]}' > "$oci/index.json"
"$owned" --insecure-policy copy "oci:$oci:latest" "docker-archive:$work/image.tar:fixture:latest"
gzip -c "$work/image.tar" > "$work/image.tar.gz"
if env -u TMPDIR "$raw" --insecure-policy copy "docker-archive:$work/image.tar.gz" "oci:$work/raw-converted:latest" > "$work/raw-output" 2> "$work/raw-error"; then
  printf 'upstream Skopeo unexpectedly escaped Darwin temporary-directory confinement\n' >&2
  exit 1
fi
grep -Fq '/var/tmp/' "$work/raw-error"
grep -Fq 'operation not permitted' "$work/raw-error"
"$owned" --insecure-policy copy "docker-archive:$work/image.tar" "oci:$work/plain-converted:latest"
"$owned" --insecure-policy copy "docker-archive:$work/image.tar.gz" "oci:$work/converted:latest"
"$owned" inspect --raw "oci:$work/plain-converted:latest" > "$work/plain-manifest"
"$owned" inspect --raw "oci:$work/converted:latest" > "$work/compressed-manifest"
cmp "$work/plain-manifest" "$work/compressed-manifest"
jq -e '.config.digest | startswith("sha256:")' "$work/compressed-manifest" > /dev/null
if env -u TMPDIR "$owned" inspect --raw "docker-archive:$work/image.tar.gz" > "$work/absent-output" 2> "$work/absent-error"; then
  printf 'owned Skopeo accepted an absent temporary-directory owner\n' >&2
  exit 1
fi
grep -Fq 'TMPDIR' "$work/absent-error"
