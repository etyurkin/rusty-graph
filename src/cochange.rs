//! Temporal coupling mined from git history: files that repeatedly change in the
//! same commit are coupled, even when there's no static edge between them. This
//! surfaces hidden dependencies (a constant and its consumer, a schema and its
//! migration) that pure static analysis can't see.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Serialize;

/// Commits touching more files than this are treated as sweeping refactors and
/// skipped for pairwise coupling — they'd otherwise couple everything to
/// everything and drown the signal.
const MAX_FILES_PER_COMMIT: usize = 40;

/// Cap on reported pairs.
const MAX_PAIRS: usize = 100;

#[derive(Debug, Serialize)]
pub struct CochangePair {
    pub a: String,
    pub b: String,
    /// Commits that changed both files.
    pub together: usize,
    /// together / min(changes(a), changes(b)) — how reliably one implies the other.
    pub confidence: f32,
}

#[derive(Debug, Serialize)]
pub struct CochangeReport {
    pub commits: usize,
    pub pairs: Vec<CochangePair>,
}

/// Parse `git log --name-only` output where each commit is preceded by a line
/// `@<hash>`. Returns the set of files changed per commit.
fn parse_log(text: &str) -> Vec<Vec<String>> {
    let mut commits: Vec<Vec<String>> = Vec::new();
    let mut current: Option<Vec<String>> = None;
    for line in text.lines() {
        if let Some(_hash) = line.strip_prefix('@') {
            if let Some(files) = current.take() {
                commits.push(files);
            }
            current = Some(Vec::new());
        } else if !line.trim().is_empty() {
            if let Some(files) = current.as_mut() {
                files.push(line.trim().to_string());
            }
        }
    }
    if let Some(files) = current.take() {
        commits.push(files);
    }
    commits
}

/// Compute co-change pairs from parsed commits. Pure function for testability.
fn couple(commits: &[Vec<String>], min: usize) -> Vec<CochangePair> {
    let mut file_counts: HashMap<&str, usize> = HashMap::new();
    let mut pair_counts: HashMap<(&str, &str), usize> = HashMap::new();

    for files in commits {
        if files.len() > MAX_FILES_PER_COMMIT {
            continue;
        }
        // Unique, sorted file list for stable unordered pairs.
        let mut uniq: Vec<&str> = files.iter().map(|s| s.as_str()).collect();
        uniq.sort_unstable();
        uniq.dedup();
        for f in &uniq {
            *file_counts.entry(f).or_insert(0) += 1;
        }
        for i in 0..uniq.len() {
            for j in (i + 1)..uniq.len() {
                *pair_counts.entry((uniq[i], uniq[j])).or_insert(0) += 1;
            }
        }
    }

    let mut pairs: Vec<CochangePair> = pair_counts
        .into_iter()
        .filter(|(_, c)| *c >= min)
        .map(|((a, b), together)| {
            let denom = file_counts[a].min(file_counts[b]).max(1) as f32;
            CochangePair {
                a: a.to_string(),
                b: b.to_string(),
                together,
                confidence: together as f32 / denom,
            }
        })
        .collect();
    pairs.sort_by(|x, y| {
        y.together
            .cmp(&x.together)
            .then(
                y.confidence
                    .partial_cmp(&x.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(x.a.cmp(&y.a))
    });
    pairs.truncate(MAX_PAIRS);
    pairs
}

pub fn analyze(project_root: &Path, since: Option<&str>, min: usize) -> Result<CochangeReport> {
    let mut args: Vec<String> = vec![
        "log".into(),
        "--no-merges".into(),
        "--pretty=format:@%H".into(),
        "--name-only".into(),
    ];
    if let Some(s) = since {
        args.push(format!("{s}..HEAD"));
    }
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let text = run_git(project_root, &arg_refs)?;
    let commits = parse_log(&text);
    let pairs = couple(&commits, min);
    Ok(CochangeReport {
        commits: commits.len(),
        pairs,
    })
}

fn run_git(project_root: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .current_dir(project_root)
        .args(args)
        .output()
        .context("failed to run git (is it installed and on PATH?)")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

impl CochangeReport {
    pub fn format(&self) -> String {
        let mut out = format!(
            "Co-change coupling over {} commits: {} pairs\n",
            self.commits,
            self.pairs.len()
        );
        for p in &self.pairs {
            out.push_str(&format!(
                "  {:>3}× ({:.0}%)  {}  ⇄  {}\n",
                p.together,
                p.confidence * 100.0,
                p.a,
                p.b
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commits_and_files() {
        let log = "@abc123\nsrc/a.rs\nsrc/b.rs\n\n@def456\nsrc/a.rs\n";
        let commits = parse_log(log);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0], vec!["src/a.rs", "src/b.rs"]);
        assert_eq!(commits[1], vec!["src/a.rs"]);
    }

    #[test]
    fn couples_files_that_change_together() {
        let commits = vec![
            vec!["a".to_string(), "b".to_string()],
            vec!["a".to_string(), "b".to_string()],
            vec!["a".to_string()],
        ];
        let pairs = couple(&commits, 2);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].together, 2);
        // a changed 3×, b changed 2×; confidence = 2/min(3,2) = 1.0.
        assert!((pairs[0].confidence - 1.0).abs() < 1e-6);
    }

    #[test]
    fn min_support_filters_weak_pairs() {
        let commits = vec![vec!["a".to_string(), "b".to_string()]];
        assert!(couple(&commits, 2).is_empty());
        assert_eq!(couple(&commits, 1).len(), 1);
    }

    #[test]
    fn sweeping_commits_are_ignored() {
        let big: Vec<String> = (0..50).map(|i| format!("f{i}")).collect();
        let commits = vec![big];
        assert!(couple(&commits, 1).is_empty());
    }
}
