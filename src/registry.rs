//! Read-only script registry — execute-by-key (Phase A of
//! `docs/design/script-registry.md`).
//!
//! Loaded once at startup from `scripts_dir`: every `*.js` file under the directory
//! becomes a registered script whose key is its relative path without the extension
//! (`acme/billing/pricing.js` → `acme/billing/pricing`). The map is immutable at
//! runtime — registration is a deploy-time concern (image layer, mounted volume), so
//! the service stays stateless and replicas stay trivially consistent.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Immutable key → script-source map, loaded at startup.
#[derive(Debug, Default)]
pub(crate) struct ScriptRegistry {
    /// Registered scripts: key (relative path, no extension) → source.
    scripts: HashMap<String, Arc<str>>,
}

impl ScriptRegistry {
    /// Loads every `*.js` file under `dir`, recursively.
    ///
    /// Each script is validated against `max_script_size` here, at load — a too-large
    /// registered script is a startup error, never a runtime surprise.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or a file can't be read, a script exceeds
    /// `max_script_size`, or a script path isn't valid UTF-8.
    pub(crate) fn load(
        dir: &Path,
        max_script_size: usize,
    ) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut scripts = HashMap::new();
        let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
        while let Some(current) = pending.pop() {
            for entry in fs::read_dir(&current)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|ext| ext == "js") {
                    let source = fs::read_to_string(&path)?;
                    if source.len() > max_script_size {
                        return Err(format!(
                            "registered script {} is {} bytes (max_script_size is {})",
                            path.display(),
                            source.len(),
                            max_script_size,
                        )
                        .into());
                    }
                    drop(scripts.insert(derive_key(dir, &path)?, Arc::from(source)));
                }
            }
        }
        Ok(Self { scripts })
    }

    /// Looks up a registered script by key.
    pub(crate) fn get(&self, key: &str) -> Option<Arc<str>> {
        self.scripts.get(key).map(Arc::clone)
    }

    /// Number of registered scripts.
    pub(crate) fn count(&self) -> usize {
        self.scripts.len()
    }
}

/// Derives the registry key for `path`: its path relative to `root`, `/`-separated,
/// without the `.js` extension.
fn derive_key(root: &Path, path: &Path) -> Result<String, Box<dyn Error + Send + Sync>> {
    let relative = path.strip_prefix(root)?.with_extension("");
    let parts = relative
        .components()
        .map(|comp| comp.as_os_str().to_str().ok_or("script path is not valid UTF-8"))
        .collect::<Result<Vec<&str>, &str>>()?;
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    //! Registry load/lookup tests against a real temp directory.

    use super::ScriptRegistry;
    use std::env;
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;

    /// Boxed error alias to keep test signatures short.
    type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

    /// Creates a unique temp fixture dir with nested scripts and one non-JS file.
    fn fixture_dir(tag: &str) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
        let dir = env::temp_dir().join(format!("jsbox-registry-test-{tag}"));
        let nested = dir.join("acme").join("billing");
        fs::create_dir_all(&nested)?;
        fs::write(dir.join("greet.js"), "function handler(ctx) { return json(1, null); }")?;
        fs::write(nested.join("pricing.js"), "function handler(ctx) { return json(2, null); }")?;
        fs::write(dir.join("notes.txt"), "not a script")?;
        Ok(dir)
    }

    /// Nested `.js` files load under `/`-separated keys; non-JS files are ignored.
    #[test]
    fn loads_nested_keys() -> TestResult {
        let dir = fixture_dir("nested")?;
        let registry = ScriptRegistry::load(&dir, 1024)?;
        assert_eq!(registry.count(), 2, "exactly the two .js files load");
        assert!(registry.get("greet").is_some(), "top-level key resolves");
        assert!(registry.get("acme/billing/pricing").is_some(), "nested key resolves");
        assert!(registry.get("notes").is_none(), "non-JS files are not registered");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// An unknown key resolves to `None`.
    #[test]
    fn unknown_key_is_none() -> TestResult {
        let dir = fixture_dir("unknown")?;
        let registry = ScriptRegistry::load(&dir, 1024)?;
        assert!(registry.get("no/such/script").is_none(), "unknown key resolves to None");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// A script over `max_script_size` fails the whole load (startup error).
    #[test]
    fn oversized_script_fails_load() -> TestResult {
        let dir = fixture_dir("oversized")?;
        assert!(ScriptRegistry::load(&dir, 8).is_err(), "oversized script must fail the load");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// The default registry is empty and resolves nothing.
    #[test]
    fn default_is_empty() {
        let registry = ScriptRegistry::default();
        assert_eq!(registry.count(), 0, "default registry holds no scripts");
        assert!(registry.get("greet").is_none(), "default registry resolves nothing");
    }
}
