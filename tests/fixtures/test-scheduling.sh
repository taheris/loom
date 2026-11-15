#!/usr/bin/env bash
set -euo pipefail

scheduler=$(realpath -e -- "$1")
# The sourced file only defines functions; it does not start a test campaign.
# shellcheck source=/dev/null
source "$scheduler"
[[ "$(loom_test_threads 2 8589934592)" == 8 ]]
[[ "$(loom_test_threads 2 4294967296)" == 2 ]]
[[ "$(loom_test_threads 2 2147483648)" == 1 ]]
[[ "$(loom_test_threads 16 68719476736)" == 16 ]]
[[ "$(loom_test_threads 3 0)" == 3 ]]
for arguments in '0 8589934592' '2 -1' 'not-a-number 1'; do
  read -r cpus memory <<< "$arguments"
  status=0
  loom_test_threads "$cpus" "$memory" > /dev/null || status=$?
  [[ "$status" == 64 ]]
done
actual=$(bash "$scheduler")
[[ "$actual" =~ ^[1-9][0-9]*$ ]]
