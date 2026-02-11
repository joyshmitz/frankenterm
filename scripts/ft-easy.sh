#!/usr/bin/env bash
set -euo pipefail

FT_BIN="${FT_BIN:-ft}"
FT_FORMAT="${FT_FORMAT:-toon}"

usage() {
  cat <<'EOF'
ft-easy: thin wrapper for high-frequency FrankenTerm operations.

Usage:
  ft-easy daystart
  ft-easy now
  ft-easy tail <pane_id> [lines]
  ft-easy safe-send <pane_id> <command> [pattern] [timeout_secs]
  ft-easy fix <pane_id> [command] [pattern] [timeout_secs]

Examples:
  ft-easy now
  ft-easy tail 3 120
  ft-easy safe-send 3 "cargo test -- --nocapture"
  ft-easy safe-send 3 "cargo test -- --nocapture" "test result:" 1800
  ft-easy fix 3
  ft-easy fix 3 "cargo check --all-targets"
  ft-easy fix 3 "cargo check --all-targets" "Finished" 900
EOF
}

require_ft_bin() {
  if ! command -v "$FT_BIN" >/dev/null 2>&1; then
    echo "ft-easy error: FT binary not found: $FT_BIN" >&2
    echo "Set FT_BIN=<path-to-ft> or add 'ft' to PATH." >&2
    exit 127
  fi
}

robot() {
  "$FT_BIN" robot --format "$FT_FORMAT" "$@"
}

cmd_daystart() {
  exec "$FT_BIN" watch --auto-handle --foreground
}

cmd_now() {
  echo "== state =="
  robot state
  echo
  echo "== unhandled events =="
  robot events --unhandled --limit 30
}

cmd_tail() {
  local pane_id="$1"
  local lines="${2:-120}"
  robot get-text "$pane_id" --tail "$lines"
}

cmd_safe_send() {
  local pane_id="$1"
  local command="$2"
  local pattern="${3:-}"
  local timeout="${4:-600}"

  echo "== dry-run =="
  robot send "$pane_id" "$command" --dry-run
  echo
  echo "== execute =="
  if [[ -n "$pattern" ]]; then
    robot send "$pane_id" "$command" --wait-for "$pattern" --timeout-secs "$timeout"
  else
    robot send "$pane_id" "$command"
  fi
}

cmd_fix() {
  local pane_id="$1"
  local command="${2:-}"
  local pattern="${3:-}"
  local timeout="${4:-600}"

  echo "== context =="
  robot get-text "$pane_id" --tail 150
  echo
  echo "== events =="
  robot events --pane "$pane_id" --unhandled --limit 20
  echo
  echo "== search =="
  robot search "error OR panic OR failed OR timeout" --pane "$pane_id" --limit 20

  if [[ -n "$command" ]]; then
    echo
    cmd_safe_send "$pane_id" "$command" "$pattern" "$timeout"
  fi
}

main() {
  local sub="${1:-help}"
  case "$sub" in
    daystart)
      cmd_daystart
      ;;
    now)
      cmd_now
      ;;
    tail)
      [[ $# -ge 2 ]] || {
        usage
        exit 2
      }
      cmd_tail "$2" "${3:-120}"
      ;;
    safe-send)
      [[ $# -ge 3 ]] || {
        usage
        exit 2
      }
      cmd_safe_send "$2" "$3" "${4:-}" "${5:-600}"
      ;;
    fix)
      [[ $# -ge 2 ]] || {
        usage
        exit 2
      }
      cmd_fix "$2" "${3:-}" "${4:-}" "${5:-600}"
      ;;
    help|-h|--help)
      usage
      ;;
    *)
      echo "Unknown subcommand: $sub" >&2
      usage
      exit 2
      ;;
  esac
}

require_ft_bin
main "$@"
