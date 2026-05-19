use anyhow::{Result, anyhow};
use glob::Pattern;

/// Compiled set of include patterns for filtering TOC paths.
///
/// A path matches if it equals the pattern, lives under it as a
/// directory prefix, or matches as a shell glob (`*`, `?`, `[...]`).
/// An empty filter matches every path (the "no positional args, take
/// everything" case used by `tar`-style CLIs).
///
/// Patterns and paths are normalized for matching by trimming any
/// leading `./` and trailing `/`, so `./src/` and `src` are treated
/// as the same prefix.
pub struct PathFilter {
    raw: Vec<String>,
    compiled: Vec<Pattern>,
}

impl PathFilter {
    /// Compile the given patterns. Returns an error if any pattern is
    /// not a valid shell glob.
    pub fn new(patterns: &[String]) -> Result<Self> {
        let compiled = patterns
            .iter()
            .map(|s| {
                Pattern::new(normalize(s))
                    .map_err(|e| anyhow!("invalid pattern `{s}`: {e}"))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            raw: patterns.to_vec(),
            compiled,
        })
    }

    /// True when the filter has no patterns (and therefore matches
    /// every path).
    pub fn is_empty(&self) -> bool {
        self.raw.is_empty()
    }

    /// Returns true if `path` matches at least one pattern, or if the
    /// filter is empty.
    pub fn matches(&self, path: &str) -> bool {
        if self.raw.is_empty() {
            return true;
        }
        let p = normalize(path);
        for (raw, glob) in self.raw.iter().zip(&self.compiled) {
            let r = normalize(raw);
            if p == r || p.starts_with(&format!("{r}/")) || glob.matches(p) {
                return true;
            }
        }
        false
    }
}

fn normalize(s: &str) -> &str {
    s.trim_start_matches("./").trim_end_matches('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pf(patterns: &[&str]) -> PathFilter {
        let owned: Vec<String> = patterns.iter().map(|s| (*s).to_owned()).collect();
        PathFilter::new(&owned).unwrap()
    }

    #[test]
    fn empty_matches_everything() {
        let f = pf(&[]);
        assert!(f.is_empty());
        assert!(f.matches("anything"));
        assert!(f.matches("./a/b/c"));
    }

    #[test]
    fn exact_match() {
        let f = pf(&["src/main.rs"]);
        assert!(f.matches("src/main.rs"));
        assert!(f.matches("./src/main.rs"));
        assert!(!f.matches("src/lib.rs"));
    }

    #[test]
    fn directory_prefix_match() {
        let f = pf(&["src/"]);
        assert!(f.matches("src/main.rs"));
        assert!(f.matches("src/format/toc.rs"));
        assert!(f.matches("src"));
        assert!(!f.matches("tests/foo.rs"));
    }

    #[test]
    fn glob_match() {
        let f = pf(&["*.toml"]);
        assert!(f.matches("Cargo.toml"));
        assert!(!f.matches("README.md"));
    }

    #[test]
    fn multiple_patterns_or_together() {
        let f = pf(&["src/", "*.toml"]);
        assert!(f.matches("src/main.rs"));
        assert!(f.matches("Cargo.toml"));
        assert!(!f.matches("README.md"));
    }

    #[test]
    fn invalid_pattern_errors() {
        let bad = "[unclosed".to_owned();
        assert!(PathFilter::new(&[bad]).is_err());
    }
}
