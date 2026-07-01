use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "rusty-graph",
    about = "Local code knowledge graph for AI coding agents",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Initialize a project and build the graph
    Init {
        /// Project path (defaults to current directory)
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Remove the project index
    Uninit {
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Full re-index of a project
    Index {
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Force re-index even if up to date
        #[arg(long)]
        force: bool,

        /// Suppress output
        #[arg(long, short)]
        quiet: bool,
    },

    /// Incremental sync (only changed files)
    Sync {
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Watch the project and incrementally re-index on file changes
    Watch {
        #[arg(default_value = ".")]
        path: PathBuf,
    },

    /// Show project index statistics
    Status {
        #[arg(default_value = ".")]
        path: PathBuf,

        /// Include a health report: language/kind breakdown, unresolved ratio,
        /// and files that failed to fully parse.
        #[arg(long)]
        health: bool,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Search for symbols
    Query {
        /// Search term
        search: String,

        /// Project path
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Filter by node kind (function, class, struct, etc.)
        #[arg(long)]
        kind: Option<String>,

        /// Maximum results
        #[arg(long, default_value = "20")]
        limit: usize,

        /// Output as JSON
        #[arg(long)]
        json: bool,
    },

    /// Assemble a token-budgeted context pack: the smallest ranked set of
    /// symbols + snippets that answers a query, for feeding to an AI agent.
    Context {
        /// Task or question (matched via smart search, then graph-expanded)
        query: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Token budget for the pack
        #[arg(long, default_value = "8000")]
        budget: usize,

        #[arg(long)]
        json: bool,
    },

    /// List the tests that transitively cover a symbol
    Tests {
        symbol: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// How deep to walk the caller chain looking for tests
        #[arg(long, default_value = "6")]
        depth: usize,

        #[arg(long)]
        json: bool,
    },

    /// Architecture report: cycles, hotspots, orphans, layer coupling
    Arch {
        #[arg(default_value = ".")]
        path: PathBuf,

        #[arg(long)]
        json: bool,
    },

    /// Export the graph (dot | json | csv | lsif)
    Export {
        /// Output format
        #[arg(long, default_value = "json")]
        format: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Temporal co-change coupling mined from git history
    Cochange {
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Only report pairs that changed together at least this many times
        #[arg(long, default_value = "3")]
        min: usize,

        /// Limit git history to commits since this ref (e.g. HEAD~200)
        #[arg(long)]
        since: Option<String>,

        #[arg(long)]
        json: bool,
    },

    /// Resolve a symbol's definition via a configured language server (LSP)
    Definition {
        /// Source file (relative to the project)
        file: String,
        /// 1-based line
        line: u32,
        /// 1-based column
        column: u32,

        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Full exploration: source + callers + blast radius (keyword/FTS search)
    Explore {
        /// Query (symbol name or keywords; matched via full-text search)
        query: String,

        /// Project path
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// Show a single symbol (source + callers) or file (with line numbers)
    Node {
        /// Symbol name or file path
        symbol: String,

        /// Project path
        #[arg(long, default_value = ".")]
        path: PathBuf,
    },

    /// List what calls a symbol
    Callers {
        symbol: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        #[arg(long, default_value = "20")]
        limit: usize,

        #[arg(long)]
        json: bool,
    },

    /// List what a symbol calls
    Callees {
        symbol: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        #[arg(long, default_value = "20")]
        limit: usize,

        #[arg(long)]
        json: bool,
    },

    /// Find a call path from one symbol to another
    Path {
        /// Caller symbol (path start)
        from: String,

        /// Callee symbol (path end)
        to: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        #[arg(long)]
        json: bool,
    },

    /// Show blast radius (transitive callers) of a symbol
    Impact {
        symbol: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        #[arg(long, default_value = "5")]
        depth: usize,

        #[arg(long)]
        json: bool,
    },

    /// Show file structure
    Files {
        #[arg(default_value = ".")]
        path: PathBuf,

        #[arg(long, default_value = "3")]
        max_depth: usize,

        #[arg(long)]
        json: bool,
    },

    /// Show symbols impacted by changes since a git ref (and their callers)
    Diff {
        /// Git ref to compare against (e.g. HEAD, main, a commit SHA)
        #[arg(default_value = "HEAD")]
        base: String,

        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Caller depth for the blast radius
        #[arg(long, default_value = "2")]
        depth: usize,

        /// Also list the tests that cover the impacted symbols
        #[arg(long)]
        tests: bool,

        #[arg(long)]
        json: bool,
    },

    /// Serve a JSON API and an interactive graph explorer over HTTP
    Serve {
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Port to listen on
        #[arg(long, default_value = "7878")]
        port: u16,
    },

    /// Start the MCP server (stdio transport)
    Mcp {
        #[arg(long, default_value = ".")]
        path: PathBuf,

        /// Comma-separated list of additional tools to expose (besides rusty_graph_explore)
        #[arg(long, env = "RUSTY_GRAPH_MCP_TOOLS")]
        tools: Option<String>,
    },
}
