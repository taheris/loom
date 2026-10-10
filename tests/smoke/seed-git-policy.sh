#!/usr/bin/env bash
set -euo pipefail

cat >wrix.toml <<'TOML'
[wrix.git]
deploy = true
sign = true
TOML
