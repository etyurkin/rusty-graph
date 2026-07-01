//! Dependency-free local embeddings for semantic-ish symbol search. We hash
//! identifier sub-tokens (camelCase / snake_case aware) into a fixed-width
//! vector and compare with cosine similarity. This is a hashed bag-of-subtokens
//! model, not a neural net — but it runs fully offline with zero model download
//! and meaningfully widens recall for natural-language queries (e.g. "validate
//! token" → `validateToken`, `TokenValidator`) that exact/FTS search misses.

use crate::types::Node;

/// Embedding width. Wide enough to keep hash collisions rare for typical
/// codebases, small enough to keep cosine cheap.
pub const DIM: usize = 256;

/// FNV-1a — small, fast, deterministic across runs and platforms.
fn hash_token(tok: &str) -> usize {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in tok.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    (h % DIM as u64) as usize
}

/// Split a raw word at camelCase and letter/digit boundaries.
fn split_identifier(word: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = word.chars().collect();
    let mut start = 0;
    for i in 1..chars.len() {
        let prev = chars[i - 1];
        let cur = chars[i];
        let boundary = (prev.is_lowercase() && cur.is_uppercase())
            || (prev.is_alphabetic() && cur.is_numeric())
            || (prev.is_numeric() && cur.is_alphabetic());
        if boundary {
            out.push(chars[start..i].iter().collect());
            start = i;
        }
    }
    if start < chars.len() {
        out.push(chars[start..].iter().collect());
    }
}

/// Lowercased sub-tokens of `text`, length ≥ 2.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut raw: Vec<String> = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        for part in word.split('_') {
            if !part.is_empty() {
                split_identifier(part, &mut raw);
            }
        }
    }
    raw.into_iter()
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 2)
        .collect()
}

fn accumulate(v: &mut [f32], text: &str, weight: f32) {
    for tok in tokenize(text) {
        v[hash_token(&tok)] += weight;
    }
}

fn normalize(v: &mut [f32]) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Embed free text (e.g. a search query).
pub fn embed_text(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    accumulate(&mut v, text, 1.0);
    normalize(&mut v);
    v
}

/// Embed a symbol, weighting its name most heavily, then qualified name,
/// signature, and docstring.
pub fn embed_node(node: &Node) -> Vec<f32> {
    let mut v = vec![0.0f32; DIM];
    accumulate(&mut v, &node.name, 3.0);
    accumulate(&mut v, &node.qualified_name, 1.0);
    if let Some(sig) = &node.signature {
        accumulate(&mut v, sig, 1.0);
    }
    if let Some(doc) = &node.docstring {
        accumulate(&mut v, doc, 1.0);
    }
    normalize(&mut v);
    v
}

/// Cosine similarity of two equal-length, L2-normalized vectors (== dot product).
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Pack a vector into little-endian bytes for BLOB storage.
pub fn to_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for x in v {
        out.extend_from_slice(&x.to_le_bytes());
    }
    out
}

/// Unpack bytes written by `to_bytes`. Returns `None` if the length is wrong.
pub fn from_bytes(bytes: &[u8]) -> Option<Vec<f32>> {
    if bytes.len() != DIM * 4 {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_splits_camel_and_snake() {
        let mut t = tokenize("validateToken");
        t.sort();
        assert_eq!(t, vec!["token", "validate"]);
        // Letter→digit is a boundary, so `id2` → `id` + `2`; the 1-char `2` is
        // dropped by the length filter.
        let mut t2 = tokenize("get_user_id2");
        t2.sort();
        assert_eq!(t2, vec!["get", "id", "user"]);
        // Longer digit runs survive: `sha256` → `sha` + `256`.
        let mut t3 = tokenize("sha256sum");
        t3.sort();
        assert_eq!(t3, vec!["256", "sha", "sum"]);
    }

    #[test]
    fn related_symbol_scores_higher_than_unrelated() {
        let q = embed_text("validate token");
        let related = embed_text("validateToken");
        let unrelated = embed_text("renderHtmlTemplate");
        assert!(
            cosine(&q, &related) > cosine(&q, &unrelated),
            "related {} vs unrelated {}",
            cosine(&q, &related),
            cosine(&q, &unrelated)
        );
    }

    #[test]
    fn bytes_round_trip() {
        let v = embed_text("hello world");
        let back = from_bytes(&to_bytes(&v)).unwrap();
        assert_eq!(v.len(), back.len());
        for (a, b) in v.iter().zip(&back) {
            assert!((a - b).abs() < 1e-6);
        }
        assert!(from_bytes(&[0, 1, 2]).is_none());
    }
}
