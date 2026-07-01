//! Svelte / Vue single-file components. These are template formats whose code
//! intelligence lives in their `<script>` blocks, so we extract each script
//! block and delegate to the JavaScript/TypeScript extractor, offsetting line
//! numbers back to the original file. This avoids depending on the (often
//! ABI-stale) Svelte/Vue tree-sitter grammars while still capturing the symbols
//! and calls that matter.

use anyhow::Result;
use std::path::Path;

use super::javascript::JsExtractor;
use super::{ExtractionResult, Extractor};
use crate::types::{Node, NodeKind};

pub struct WebComponentExtractor {
    pub language: &'static str,
}

impl Extractor for WebComponentExtractor {
    fn language(&self) -> &'static str {
        self.language
    }

    fn extract(&self, path: &Path, source: &str) -> Result<ExtractionResult> {
        let mut result = ExtractionResult::empty();
        let mut have_file_node = false;

        for block in script_blocks(source) {
            let inner = JsExtractor.extract(path, block.code)?;
            for mut node in inner.nodes {
                match node.kind {
                    NodeKind::File => {
                        // Keep a single file node, relabelled to the component
                        // language and spanning the whole file.
                        if have_file_node {
                            continue;
                        }
                        have_file_node = true;
                        node.language = self.language.to_string();
                        node.start_line = 1;
                        node.end_line = source.lines().count() as u32;
                        result.nodes.push(node);
                    }
                    _ => {
                        node.language = self.language.to_string();
                        node.start_line += block.line_offset;
                        node.end_line += block.line_offset;
                        result.nodes.push(node);
                    }
                }
            }
            result.edges.extend(inner.edges);
            result.unresolved.extend(inner.unresolved);
        }

        if !have_file_node {
            result.nodes.push(file_node(path, source, self.language));
        }
        Ok(result)
    }
}

struct ScriptBlock<'a> {
    code: &'a str,
    line_offset: u32,
}

/// Extract the contents of every `<script>…</script>` block, with the line
/// number of the first code line (used to offset reported positions).
fn script_blocks(source: &str) -> Vec<ScriptBlock<'_>> {
    let mut blocks = vec![];
    let bytes = source.as_bytes();
    let mut search = 0;
    while let Some(rel) = source[search..].find("<script") {
        let tag_start = search + rel;
        // Find the end of the opening tag '>'.
        let Some(gt_rel) = source[tag_start..].find('>') else {
            break;
        };
        let code_start = tag_start + gt_rel + 1;
        let Some(end_rel) = source[code_start..].find("</script>") else {
            break;
        };
        let code_end = code_start + end_rel;
        let code = &source[code_start..code_end];
        // Lines before code_start = number of newlines in source[..code_start].
        let line_offset = bytecount_newlines(&bytes[..code_start]);
        blocks.push(ScriptBlock { code, line_offset });
        search = code_end + "</script>".len();
    }
    blocks
}

fn bytecount_newlines(bytes: &[u8]) -> u32 {
    bytes.iter().filter(|&&b| b == b'\n').count() as u32
}

fn file_node(path: &Path, source: &str, language: &'static str) -> Node {
    let file_path = path.to_string_lossy().to_string();
    Node {
        id: Node::new_id(&file_path, &file_path),
        kind: NodeKind::File,
        name: path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
        qualified_name: file_path.clone(),
        file_path,
        language: language.to_string(),
        start_line: 1,
        end_line: source.lines().count() as u32,
        signature: None,
        docstring: None,
        visibility: None,
        is_exported: false,
        is_async: false,
        is_static: false,
        is_abstract: false,
    }
}
