#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
grammar_dir="${root}/vendor/tree-sitter-kotlin-ng"

if ! command -v tree-sitter >/dev/null 2>&1; then
  echo "tree-sitter CLI is required (https://tree-sitter.github.io/tree-sitter/cli/install.html)" >&2
  exit 1
fi

cd "${grammar_dir}"
tree-sitter generate
tree-sitter test

echo "Regenerated parser.c and corpus tests passed."
