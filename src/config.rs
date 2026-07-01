use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Default cap on the size of a file the indexer will parse (1 MB). Larger files
/// are usually generated or vendored and blow up parse time for little value.
pub const DEFAULT_MAX_FILE_SIZE: u64 = 1_024 * 1_024;

/// Per-project indexing configuration, loaded from
/// `<project>/.rusty-graph/config.json` if present. All fields are optional and
/// fall back to sensible defaults, so the file itself is optional too.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Skip files larger than this many bytes.
    pub max_file_size: u64,
    /// Languages to skip entirely (e.g. `["scheme", "elisp"]`).
    pub disabled_languages: Vec<String>,
    /// Extra roots to index as external dependencies (e.g. `["vendor", "../lib"]`),
    /// so calls into them resolve instead of dead-ending at the project boundary.
    pub extra_roots: Vec<String>,
    /// Per-language language-server command for the optional LSP bridge,
    /// e.g. `{ "rust": "rust-analyzer", "python": "pyright-langserver --stdio" }`.
    pub lsp: HashMap<String, String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_file_size: DEFAULT_MAX_FILE_SIZE,
            disabled_languages: Vec::new(),
            extra_roots: Vec::new(),
            lsp: HashMap::new(),
        }
    }
}

impl Config {
    /// Load configuration for `project_root`, falling back to defaults when the
    /// file is absent. A present-but-invalid file is reported and ignored rather
    /// than aborting indexing.
    pub fn load(project_root: &Path) -> Self {
        let path = project_root.join(".rusty-graph").join("config.json");
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("Ignoring invalid config {}: {}", path.display(), e);
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn language_enabled(&self, language: &str) -> bool {
        !self.disabled_languages.iter().any(|l| l == language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.max_file_size, DEFAULT_MAX_FILE_SIZE);
        assert!(cfg.language_enabled("rust"));
    }

    #[test]
    fn loads_overrides_from_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rusty-graph")).unwrap();
        std::fs::write(
            dir.path().join(".rusty-graph").join("config.json"),
            r#"{ "max_file_size": 2048, "disabled_languages": ["scheme"] }"#,
        )
        .unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.max_file_size, 2048);
        assert!(!cfg.language_enabled("scheme"));
        assert!(cfg.language_enabled("rust"));
    }

    #[test]
    fn invalid_json_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".rusty-graph")).unwrap();
        std::fs::write(
            dir.path().join(".rusty-graph").join("config.json"),
            "{ not valid json",
        )
        .unwrap();
        let cfg = Config::load(dir.path());
        assert_eq!(cfg.max_file_size, DEFAULT_MAX_FILE_SIZE);
    }
}
