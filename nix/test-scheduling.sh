#!/usr/bin/env bash
set -euo pipefail

loom_test_threads() {
  local cpus="$1" memory="$2" threads ceiling slots
  [[ "$cpus" =~ ^[1-9][0-9]*$ && "$memory" =~ ^[0-9]+$ ]] || return 64
  # Native integration fixtures mostly wait for real local processes and I/O.
  # Increase overlap on small workers, bounded by memory and a modest CPU cap.
  threads=$((cpus * 4))
  ceiling=8
  if ((cpus > ceiling)); then ceiling="$cpus"; fi
  if ((threads > ceiling)); then threads="$ceiling"; fi
  if ((memory == 0)); then
    threads="$cpus"
  else
    # Leave two GiB for compilation/the agent, and 768 MiB per executing test.
    slots=$(((memory - 2147483648) / 805306368))
    if ((slots < 1)); then slots=1; fi
    if ((threads > slots)); then threads="$slots"; fi
  fi
  printf '%s\n' "$threads"
}

loom_test_resources() {
  local cpus memory=0 key value unit hierarchy controllers group directory limit
  cpus=$(nproc)
  if [[ -r /proc/meminfo ]]; then
    while read -r key value unit; do
      if [[ "$key" == MemTotal: && "$value" =~ ^[0-9]+$ && "$unit" == kB ]]; then
        memory=$((value * 1024))
        break
      fi
    done < /proc/meminfo
  fi
  directory=/sys/fs/cgroup
  if [[ -r /proc/self/cgroup ]]; then
    while IFS=: read -r hierarchy controllers group; do
      if [[ "$hierarchy" == 0 && -z "$controllers" && -d "/sys/fs/cgroup$group" && "$group" != *..* ]]; then
        directory="/sys/fs/cgroup$group"
        directory=${directory%/}
        break
      fi
    done < /proc/self/cgroup
  fi
  while [[ "$directory" == /sys/fs/cgroup* ]]; do
    if [[ -r "$directory/memory.max" ]]; then
      read -r limit < "$directory/memory.max"
      if [[ "$limit" =~ ^[1-9][0-9]*$ ]] && ((memory == 0 || limit < memory)); then
        memory="$limit"
      fi
    fi
    [[ "$directory" != /sys/fs/cgroup ]] || break
    directory=$(dirname -- "$directory")
  done
  loom_test_threads "$cpus" "$memory"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  [[ "$#" == 0 ]] || exit 64
  loom_test_resources
fi
