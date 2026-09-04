//! Contributor configuration (SPEC §8).
//!
//! Local `.snap/config.json` is read first. If it provides an id, global
//! configuration is not consulted at all. A missing file means no value; a
//! malformed file, an unknown field, or an invalid id *in a file that is
//! actually read* is an error.

use crate::error::Result;
use crate::json::{self, Json};
use crate::version::validate_contributor_id;
use std::path::{Path, PathBuf};

/// SPEC §8: configuration has exactly this shape.
fn parse(text: &str) -> Result<Option<String>> {
    let value = json::parse(text)?;
    value.exact_fields(&["contributor"])?;
    let contributor = value.get("contributor").expect("checked");
    contributor.exact_fields(&["id"])?;
    let Some(Json::Str(id)) = contributor.get("id") else {
        return Err(crate::error::invalid_json(
            "contributor.id must be a string",
        ));
    };
    validate_contributor_id(id)?;
    Ok(Some(id.clone()))
}

#[must_use]
pub fn render(id: &str) -> String {
    json::to_canonical_string(&Json::Obj(vec![(
        "contributor".into(),
        Json::Obj(vec![("id".into(), Json::Str(id.to_string()))]),
    )]))
}

/// Read one configuration file. `Ok(None)` means the file is absent, which
/// SPEC §8 distinguishes from a file that exists but is unreadable or invalid.
fn read_file(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(crate::error::Error::new(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

#[must_use]
pub fn local_path(repo_root: &Path) -> PathBuf {
    repo_root.join(".snap").join("config.json")
}

/// `$HOME/.snapconfig.json`, or `None` when `$HOME` is unset — SPEC §8 says
/// global configuration is then simply unavailable, not an error.
#[must_use]
pub fn global_path(home: Option<&Path>) -> Option<PathBuf> {
    home.map(|h| h.join(".snapconfig.json"))
}

/// Resolve the contributor id with local-over-global precedence.
pub fn resolve(repo_root: Option<&Path>, home: Option<&Path>) -> Result<Option<String>> {
    if let Some(root) = repo_root {
        if let Some(id) = read_file(&local_path(root))? {
            return Ok(Some(id));
        }
    }
    match global_path(home) {
        Some(path) => read_file(&path),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_exact_documented_shape() {
        assert_eq!(
            parse(r#"{"contributor":{"id":"alice@example.com"}}"#).unwrap(),
            Some("alice@example.com".to_string())
        );
    }

    #[test]
    fn rejects_unknown_missing_and_duplicated_fields() {
        assert!(parse(r#"{"contributor":{"id":"a@x"},"extra":1}"#).is_err());
        assert!(parse(r#"{"contributor":{"id":"a@x","extra":1}}"#).is_err());
        assert!(parse(r"{}").is_err());
        assert!(parse(r#"{"contributor":{}}"#).is_err());
        assert!(parse(r#"{"contributor":{"id":"a@x","id":"b@x"}}"#).is_err());
    }

    #[test]
    fn rejects_an_invalid_id_and_malformed_json() {
        assert!(parse(r#"{"contributor":{"id":"nope"}}"#).is_err());
        assert!(parse(r#"{"contributor":{"id":1}}"#).is_err());
        assert!(parse("not json").is_err());
    }

    #[test]
    fn render_round_trips_through_parse() {
        let text = render("alice@example.com");
        assert_eq!(parse(&text).unwrap(), Some("alice@example.com".to_string()));
        assert!(text.ends_with("}\n"));
    }

    #[test]
    fn a_missing_file_is_not_an_error() {
        let missing = Path::new("/nonexistent/snap/config.json");
        assert_eq!(read_file(missing).unwrap(), None);
    }

    #[test]
    fn resolution_without_home_yields_no_value() {
        assert_eq!(resolve(None, None).unwrap(), None);
    }
}
