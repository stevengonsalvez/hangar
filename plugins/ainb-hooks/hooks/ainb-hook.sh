#!/bin/sh
# ainb-hook.sh: forward one agent hook event to the hangar daemon over
# loopback HTTP (hooks-and-answers).
#
# Installed by `ainb fleet atc setup --hooks=http`. Each managed entry runs:
#   AINB_AGENT=<agent> AINB_HOOK_EVENT=<event> AINB_MANAGED=atc '<home>/hooks/ainb-hook.sh'
#
# Contract:
# - The daemon publishes <home>/hangar/hook-endpoint.env (port, no secret) and
#   <home>/hangar/hook-headers (the token, 0600). Both are re-read on every
#   call, so a pane that outlives a daemon restart reaches the new daemon.
# - The endpoint file is PARSED line by line against an allowlist. It is never
#   sourced, so a corrupted file cannot run as shell code.
# - Before sending anything the script checks the daemon is really there and
#   really ours: the pid it published answers `kill -0`, and both files are
#   owned by this user. A stale file left by a crash, or a port another user
#   squats on, gets nothing (and no hold).
# - The token reaches curl only as `-H @<home>/hangar/hook-headers`, a fixed
#   path that must be a regular file this user owns. It never appears in argv.
# - Status events print `{}` and return within ~1.5s whatever the daemon does.
# - Claude PermissionRequest and PreToolUse go to the hold route; the daemon
#   decides which of them wait for a human (AskUserQuestion) and answers the
#   rest at once. The script prints the daemon's answer, or `{}` on any failure
#   so the agent shows its own prompt and the keyboard decides.
# - When the daemon certainly did not record a status event (unreachable,
#   nothing listening, or it answered 429/503), the event is appended to a
#   spool the daemon drains at its next start. Tool events and holds never
#   spool: a replayed approval would be a phantom.
# - Always exits 0.

if [ "$#" -gt 0 ]; then
  payload=$1
else
  payload=$(cat)
fi

agent=${AINB_AGENT:-claude}
# The same resolution as the daemon's hangar home: AINB_HANGAR_HOME, else the
# default. Nothing else, so the script never reads files the daemon did not
# write.
home=${AINB_HANGAR_HOME:-$HOME/.agents-in-a-box}
endpoint="$home/hangar/hook-endpoint.env"

# A Claude background job worker is not a session anyone watches.
if [ -n "${CLAUDE_JOB_DIR:-}" ] || [ -z "$payload" ]; then
  printf '{}\n'
  exit 0
fi

json_field() {
  # First-level string field; good enough for the fields hooks always send.
  printf '%s' "$payload" | tr '\n' ' ' |
    sed -n "s/.*\"$1\"[[:space:]]*:[[:space:]]*\"\([^\"]*\)\".*/\1/p"
}

event=${AINB_HOOK_EVENT:-}
[ -n "$event" ] || event=$(json_field hook_event_name)

safe() {
  # Keep only characters that are harmless in a file name and a JSON string.
  printf '%s' "$1" | tr -c 'A-Za-z0-9:_%-' '_' | cut -c1-96
}

# Route on the event name only. Which PreToolUse holds is the daemon's call.
# Only the managed entry's own AINB_HOOK_EVENT may open a hold; a name read
# from the payload never does.
blocking=0
if [ "$agent" = claude ] && [ -n "${AINB_HOOK_EVENT:-}" ]; then
  case "$AINB_HOOK_EVENT" in
    PermissionRequest | PreToolUse) blocking=1 ;;
  esac
fi

# The headers file is never taken from the endpoint file: it is always this
# fixed name in the hangar home, and must be a regular file, not a symlink.
headers="$home/hangar/hook-headers"
port=
pid=
version=
if [ -r "$endpoint" ]; then
  while IFS='=' read -r key value || [ -n "$key" ]; do
    case "$key" in
      AINB_HOOK_PORT)
        case "$value" in
          '' | *[!0-9]*) port= ;;
          *) port=$value ;;
        esac
        ;;
      AINB_HOOK_PID)
        # 0 and 1 are refused: `kill -0 0` always succeeds, and 1 is init.
        case "$value" in
          '' | *[!0-9]* | 0* | 1) pid= ;;
          *) pid=$value ;;
        esac
        ;;
      AINB_HOOK_VERSION)
        version=$value
        ;;
    esac
  done <"$endpoint"
fi

spool() {
  case "$event" in
    PreToolUse | PostToolUse | PostToolUseFailure) return 0 ;;
  esac
  dir="$home/hangar/hook-spool"
  (umask 077 && mkdir -p "$dir") 2>/dev/null || return 0
  key=${AINB_PANE_KEY:-}
  [ -n "$key" ] || key=$(json_field session_id)
  # Same character set as the daemon's spool_stem: no path separators.
  stem=$(printf '%s' "$key" | tr -c 'A-Za-z0-9:_-' '_' | cut -c1-96)
  [ -n "$stem" ] || stem=_
  file="$dir/$stem.jsonl"
  if [ -f "$file" ] && [ -n "$(find "$file" -mtime +7 2>/dev/null)" ]; then
    : >"$file"
  fi
  size=0
  [ -f "$file" ] && size=$(wc -c <"$file" 2>/dev/null | tr -d ' ')
  [ "${size:-0}" -lt 5242880 ] || return 0
  now=$(date +%s 2>/dev/null || printf 0)
  line=$(printf '%s' "$payload" | tr '\n' ' ')
  (
    umask 077
    printf '{"v":1,"source":"%s","event":"%s","pane_key":"%s","tmux_pane":"%s","parent":"%s","received_at_ms":%s000,"payload":%s}\n' \
      "$(safe "$agent")" "$(safe "$event")" "$(safe "${AINB_PANE_KEY:-}")" \
      "$(safe "${TMUX_PANE:-}")" "$(safe "${AINB_PARENT_SESSION:-}")" "$now" "$line" >>"$file"
  ) 2>/dev/null || :
}

# post <path> <max-time>: prints the body, then a last line with the status.
# Returns curl's own exit status (7: nothing listening).
post() {
  printf '%s' "$payload" | curl -sS -X POST "http://127.0.0.1:${port}$1" \
    --connect-timeout 0.5 --max-time "$2" --noproxy 127.0.0.1 \
    -H @"$headers" \
    -H 'Content-Type: application/json' \
    -H 'Expect:' \
    -H "X-Ainb-Pane-Key: ${AINB_PANE_KEY:-}" \
    -H "X-Ainb-Tmux-Pane: ${TMUX_PANE:-}" \
    -H "X-Ainb-Parent: ${AINB_PARENT_SESSION:-}" \
    -w '\n%{http_code}' \
    --data-binary @- 2>/dev/null
}

# Only a live daemon that this user owns: the published pid answers kill -0
# (which also fails for another user's process), and both files are ours.
# `test -O` is outside POSIX but in every sh this runs under (dash, bash,
# the macOS sh).
reachable=0
# shellcheck disable=SC3067
if [ -n "$port" ] && [ -n "$pid" ] && [ "$version" = 1 ] &&
  [ -O "$endpoint" ] && [ -f "$headers" ] && [ ! -L "$headers" ] &&
  [ -O "$headers" ] && [ -r "$headers" ] &&
  kill -0 "$pid" 2>/dev/null && command -v curl >/dev/null 2>&1; then
  reachable=1
fi

# Spool only what the daemon certainly did not record: nothing reachable,
# nothing listening (curl 7), or the daemon said so (429, 503). A timeout
# may have been recorded, so it is never replayed.
should_spool() {
  [ "$reachable" = 0 ] && return 0
  [ "$1" = 7 ] && return 0
  case "$2" in 429 | 503) return 0 ;; esac
  return 1
}

if [ "$blocking" = 1 ] || [ "$event" = Stop ]; then
  # The daemon's body is the agent's answer: a human decision for a hold, or
  # an immediate inbox decision on Stop.
  if [ "$blocking" = 1 ]; then route="/hook/$agent/hold" budget=610; else route="/hook/$agent" budget=1.5; fi
  rc=0 code=
  if [ "$reachable" = 1 ]; then
    resp=$(post "$route" "$budget") || rc=$?
    code=${resp##*
}
    body=${resp%
*}
    if [ "$rc" = 0 ] && [ "$code" = 200 ] && [ -n "$body" ]; then
      printf '%s\n' "$body"
      exit 0
    fi
  fi
  # A hold is never spooled: a replayed approval would be a phantom. Stop is
  # status and is.
  if [ "$blocking" = 0 ] && should_spool "$rc" "$code"; then spool; fi
  printf '{}\n'
  exit 0
fi

printf '{}\n'
rc=0 code=
if [ "$reachable" = 1 ]; then
  resp=$(post "/hook/$agent" 1.5) || rc=$?
  code=${resp##*
}
fi
if should_spool "$rc" "$code"; then spool; fi
exit 0
