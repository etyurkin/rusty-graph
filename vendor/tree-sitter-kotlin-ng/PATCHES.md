# Patches vs crates.io 1.1.0

## Optional class-member semicolons

**Problem:** `class_body` required `_class_member_semi` after every member, but the
external scanner only emits that token after a newline (Kotlin ASI). Compact bodies
like `class Foo { fun a() {} fun b() {} }` failed to parse.

**Fix:** In `grammar.js`, make `_class_member_semi` optional in `class_body` and
`enum_class_body`:

```js
repeat(seq($.class_member_declaration, optional($._class_member_semi))),
```

Regenerate with `tree-sitter generate` after grammar changes.

**Corpus test:** `test/corpus/compact_class_body.txt` — run `tree-sitter test` in
this directory, or from the repo root:

```bash
./scripts/regenerate-kotlin-grammar.sh
```

**Upstream patch:** `patches/tree-sitter-kotlin-optional-class-member-semi.patch`

Upstream: https://github.com/tree-sitter-grammars/tree-sitter-kotlin

### Submitting upstream

1. Fork `tree-sitter-grammars/tree-sitter-kotlin`.
2. Apply `patches/tree-sitter-kotlin-optional-class-member-semi.patch`.
3. Add `test/corpus/compact_class_body.txt` (copy from this vendor tree).
4. Run `tree-sitter generate && tree-sitter test`.
5. Open a PR with title: `Make class-member semicolons optional in class bodies`.

Once merged and published to crates.io, revert `Cargo.toml` to the crates.io
`tree-sitter-kotlin-ng` dependency and delete this vendor copy.
