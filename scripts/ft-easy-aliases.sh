# shellcheck shell=bash
#
# Source this file from ~/.zshrc or ~/.bashrc.
#
# Resolution order:
# 1) FT_EASY_BIN (explicit path)
# 2) FT_HOME/scripts/ft-easy.sh
# 3) ./scripts/ft-easy.sh (if sourced from repo root)
if [[ -z "${FT_EASY_BIN:-}" ]]; then
  if [[ -n "${FT_HOME:-}" && -x "${FT_HOME}/scripts/ft-easy.sh" ]]; then
    FT_EASY_BIN="${FT_HOME}/scripts/ft-easy.sh"
  elif [[ -x "./scripts/ft-easy.sh" ]]; then
    FT_EASY_BIN="$(pwd)/scripts/ft-easy.sh"
  else
    echo "ft-easy aliases: set FT_HOME or FT_EASY_BIN before sourcing." >&2
    # shellcheck disable=SC2317
    return 1 2>/dev/null || exit 1
  fi
fi

frstart() { "$FT_EASY_BIN" daystart "$@"; }
frnow() { "$FT_EASY_BIN" now "$@"; }
frtail() { "$FT_EASY_BIN" tail "$@"; }
frsend() { "$FT_EASY_BIN" safe-send "$@"; }
frfix() { "$FT_EASY_BIN" fix "$@"; }
