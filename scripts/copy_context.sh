#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'USAGE'
Usage: scripts/copy_context.sh [options]

Copies a text snapshot of repo context to macOS clipboard using pbcopy.

Options:
  --max-bytes N         Skip files larger than N bytes (default: 200000)
  --include-untracked   Include untracked files (default: tracked only)
  --changed             Copy only changed files (staged + unstaged)
  --path PATHSPEC       Limit files to a path/pathspec (can be passed multiple times)
  --stdout              Print snapshot to stdout instead of clipboard
  -h, --help            Show help
USAGE
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Missing required command: $1" >&2
    exit 1
  fi
}

is_text_file() {
  local f="$1"
  if [[ ! -s "$f" ]]; then
    return 0
  fi
  LC_ALL=C grep -Iq . "$f"
}

MAX_BYTES=200000
INCLUDE_UNTRACKED=0
ONLY_CHANGED=0
TO_STDOUT=0
PATHS=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --max-bytes)
      MAX_BYTES="${2:-}"
      shift 2
      ;;
    --include-untracked)
      INCLUDE_UNTRACKED=1
      shift
      ;;
    --changed)
      ONLY_CHANGED=1
      shift
      ;;
    --path)
      PATHS+=("${2:-}")
      shift 2
      ;;
    --stdout)
      TO_STDOUT=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
done

if ! [[ "$MAX_BYTES" =~ ^[0-9]+$ ]]; then
  echo "--max-bytes must be an integer" >&2
  exit 1
fi

require_cmd git

REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null || true)"
if [[ -z "$REPO_ROOT" ]]; then
  echo "Not inside a git repository." >&2
  exit 1
fi
cd "$REPO_ROOT"

if [[ "$TO_STDOUT" -eq 0 ]]; then
  require_cmd pbcopy
fi

git_files() {
  if [[ "${#PATHS[@]}" -gt 0 ]]; then
    git ls-files -- "${PATHS[@]}"
  else
    git ls-files
  fi
}

untracked_files() {
  if [[ "${#PATHS[@]}" -gt 0 ]]; then
    git ls-files --others --exclude-standard -- "${PATHS[@]}"
  else
    git ls-files --others --exclude-standard
  fi
}

changed_files() {
  if [[ "${#PATHS[@]}" -gt 0 ]]; then
    {
      git diff --name-only -- "${PATHS[@]}"
      git diff --name-only --cached -- "${PATHS[@]}"
    }
  else
    {
      git diff --name-only
      git diff --name-only --cached
    }
  fi
}

mapfile -t files < <(changed_files)

if [[ "$ONLY_CHANGED" -eq 0 ]]; then
  mapfile -t tracked < <(git_files)
  files+=("${tracked[@]:-}")
fi

if [[ "$INCLUDE_UNTRACKED" -eq 1 ]]; then
  mapfile -t untracked < <(untracked_files)
  files+=("${untracked[@]:-}")
fi

mapfile -t files < <(printf '%s\n' "${files[@]:-}" | awk 'NF' | sort -u)

if [[ "${#files[@]}" -eq 0 ]]; then
  echo "No files matched selection." >&2
  exit 1
fi

tmpfile="$(mktemp)"
trap 'rm -f "$tmpfile"' EXIT

branch="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
commit="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
status_short="$(git status --short || true)"
timestamp="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"

copied=0
skipped_binary=0
skipped_large=0
skipped_missing=0

{
  echo "# Kernel Context Snapshot"
  echo
  echo "Generated: $timestamp"
  echo "Repo: $REPO_ROOT"
  echo "Branch: $branch"
  echo "Commit: $commit"
  echo
  echo "## Git Status"
  if [[ -n "$status_short" ]]; then
    echo '```text'
    printf '%s\n' "$status_short"
    echo '```'
  else
    echo "Clean working tree."
  fi
  echo
  echo "## Selected Files (${#files[@]})"
  echo '```text'
  printf '%s\n' "${files[@]}"
  echo '```'
  echo

  for file in "${files[@]}"; do
    if [[ ! -f "$file" ]]; then
      ((skipped_missing+=1))
      continue
    fi

    bytes="$(wc -c < "$file" | tr -d ' ')"
    if (( bytes > MAX_BYTES )); then
      ((skipped_large+=1))
      continue
    fi

    if ! is_text_file "$file"; then
      ((skipped_binary+=1))
      continue
    fi

    ((copied+=1))
    echo "## FILE: $file"
    echo '```'
    cat -- "$file"
    echo
    echo '```'
    echo
  done

  echo "## Summary"
  echo "- Copied text files: $copied"
  echo "- Skipped binary files: $skipped_binary"
  echo "- Skipped large files (> $MAX_BYTES bytes): $skipped_large"
  echo "- Skipped missing/deleted files: $skipped_missing"
} > "$tmpfile"

if [[ "$TO_STDOUT" -eq 1 ]]; then
  cat "$tmpfile"
else
  pbcopy < "$tmpfile"
  echo "Copied kernel context to clipboard." >&2
  echo "Files copied: $copied (binary skipped: $skipped_binary, large skipped: $skipped_large, missing skipped: $skipped_missing)" >&2
fi
