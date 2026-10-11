#!/usr/bin/env bash
set -euo pipefail

cat >wrix.toml <<'TOML'
[git]
deploy = true
sign = true
TOML
