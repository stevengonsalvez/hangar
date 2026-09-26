#!/usr/bin/env bash
# Warn or fail on the size of a built artifact.
#
#   size-gate.sh <file> <warn-bytes> <fail-bytes> [label]
#
# The phone bridge has a size budget per ABI (M1-03): a library the app must
# carry on every install, so growth is a regression, not a detail. Above
# `warn-bytes` the gate prints a warning (a GitHub annotation when run in a
# workflow) and still passes; above `fail-bytes` it fails. The size is printed
# either way, so the workflow log carries the number.
set -euo pipefail

file="${1:?file}"
warn="${2:?warn-bytes}"
fail="${3:?fail-bytes}"
label="${4:-$(basename "$file")}"

if [[ ! -f "$file" ]]; then
  echo "size-gate: $file does not exist" >&2
  exit 2
fi

size=$(wc -c <"$file" | tr -d ' ')
printf '%s: %d bytes (warn above %d, fail above %d)\n' "$label" "$size" "$warn" "$fail"
if (( size > fail )); then
  echo "size-gate: $label is over the limit by $((size - fail)) bytes" >&2
  exit 1
fi
if (( size > warn )); then
  echo "::warning title=size-gate::$label is $size bytes, over the $warn byte budget; a size pass is due before the limit"
fi
