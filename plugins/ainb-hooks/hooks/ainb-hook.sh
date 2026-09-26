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
# - The token reaches curl only as `-H @<file>`. It never appears in argv.
# - Status events print `{}` and return within ~1.5s whatever the daemon does.
# - Blocking events (Claude PermissionRequest, and PreToolUse on
#   AskUserQuestion) wait for the daemon's answer and print it. On any failure
#   they print `{}`, so the agent shows its own prompt and the keyboard decides.
# - When the daemon cannot be reached, non-tool events are appended to a spool
#   the daemon drains at its next start.
# - Always exits 0.

if [ "$#" -gt 0 ]; then
  payload=$1
else
  payload=$(cat)
fi

agent=${AINB_AGENT:-claude}
home=${AINB_HANGAR_HOME:-${AINB_HOME:-$HOME/.agents-in-a-box}}
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

blocking=0
if [ "$agent" = claude ]; then
  case "$event" in
    PermissionRequest) blocking=1 ;;
    PreToolUse) [ "$(json_field tool_name)" = AskUserQuestion ] && blocking=1 ;;
  esac
fi

port=
headers=
if [ -r "$endpoint" ]; then
  while IFS='=' read -r key value || [ -n "$key" ]; do
    case "$key" in
      AINB_HOOK_PORT)
        case "$value" in
          '' | *[!0-9]*) port= ;;
          *) port=$value ;;
        esac
        ;;
      AINB_HOOK_HEADERS)
        case "$value" in
          /*[!A-Za-z0-9/._-]* | *..*) headers= ;;
          /*) headers=$value ;;
          *) headers= ;;
        esac
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

reachable=0
[ -n "$port" ] && [ -n "$headers" ] && [ -r "$headers" ] && command -v curl >/dev/null 2>&1 && reachable=1

if [ "$blocking" = 1 ] || [ "$event" = Stop ]; then
  # The daemon's body is the agent's answer: a human decision for a hold, or
  # an immediate inbox decision on Stop.
  if [ "$blocking" = 1 ]; then route="/hook/$agent/hold" budget=610; else route="/hook/$agent" budget=1.5; fi
  if [ "$reachable" = 1 ] && resp=$(post "$route" "$budget"); then
    code=${resp##*
}
    body=${resp%
*}
    if [ "$code" = 200 ] && [ -n "$body" ]; then
      printf '%s\n' "$body"
      exit 0
    fi
    printf '{}\n'
    exit 0
  fi
  spool
  printf '{}\n'
  exit 0
fi

printf '{}\n'
if [ "$reachable" = 1 ] && resp=$(post "/hook/$agent" 1.5); then
  case "${resp##*
}" in
    2??) exit 0 ;;
  esac
fi
spool
exit 0
