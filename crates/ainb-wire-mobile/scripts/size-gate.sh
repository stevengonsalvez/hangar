#!/usr/bin/env bash
# Fail when a built artifact is larger than its budget.
#
#   size-gate.sh <file> <max-bytes> [label]
#
# The phone bridge has a size budget per ABI (M1-03): a shared library the app
# must carry on every install, so growth is a regression, not a detail. Prints
# the size either way, so the workflow log carries the number.
set -euo pipefail

file="${1:?file}"
max="${2:?max-bytes}"
label="${3:-$(basename "$file")}"

if [[ ! -f "$file" ]]; then
  echo "size-gate: $file does not exist" >&2
  exit 2
fi

size=$(wc -c <"$file" | tr -d ' ')
printf '%s: %d bytes (budget %d)\n' "$label" "$size" "$max"
if (( size > max )); then
  echo "size-gate: $label is over budget by $((size - max)) bytes" >&2
  exit 1
fi
