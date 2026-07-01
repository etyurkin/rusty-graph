//! Lightweight trigram fuzzy matching, used as a fallback when exact/FTS search
//! misses (typos, partial names, different word order). No model or extra index
//! required — we score candidates by trigram Jaccard similarity in memory. This
//! keeps the "cheaper than embeddings RAG" property while still tolerating fuzzy
//! intent queries.

use std::collections::HashSet;

/// Decompose `s` into the set of its lowercased character trigrams, padded so
/// short strings still produce grams. Non-alphanumeric characters are treated as
/// boundaries (replaced with spaces) so `get_user` and `getUser` align.
fn trigrams(s: &str) -> HashSet<[char; 3]> {
    let normalized: String = s
        .chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect();
    let padded: Vec<char> = format!("  {}  ", normalized.trim()).chars().collect();
    let mut set = HashSet::new();
    if padded.len() < 3 {
        return set;
    }
    for w in padded.windows(3) {
        set.insert([w[0], w[1], w[2]]);
    }
    set
}

/// Jaccard similarity of two strings' trigram sets, in `[0.0, 1.0]`.
pub fn similarity(a: &str, b: &str) -> f32 {
    let ta = trigrams(a);
    let tb = trigrams(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let inter = ta.intersection(&tb).count() as f32;
    let union = ta.union(&tb).count() as f32;
    inter / union
}

/// Score `query` against a symbol's `name` and `qualified_name`, returning the
/// stronger of the two signals plus a bonus for substring containment (so a
/// short query that is a clean prefix/substring ranks above a noisy fuzzy hit).
pub fn score(query: &str, name: &str, qualified_name: &str) -> f32 {
    let q = query.to_ascii_lowercase();
    let base = similarity(&q, name).max(similarity(&q, qualified_name));
    let contains_bonus = if name.to_ascii_lowercase().contains(&q) {
        0.3
    } else {
        0.0
    };
    (base + contains_bonus).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_strings_are_maximally_similar() {
        assert!((similarity("getUser", "getUser") - 1.0).abs() < 1e-6);
    }

    #[test]
    fn typo_is_still_close() {
        assert!(similarity("getUser", "getUsr") > 0.4);
    }

    #[test]
    fn unrelated_strings_score_low() {
        assert!(similarity("getUser", "shutdownReactor") < 0.2);
    }

    #[test]
    fn substring_query_gets_a_bonus() {
        // A short query that is contained should beat a fuzzy-only match.
        let contained = score("user", "createUser", "svc::createUser");
        let fuzzy = score("user", "shutdown", "svc::shutdown");
        assert!(contained > fuzzy);
    }
}
