//! Injectable ES modules — operator-authored JS libraries a handler can `import`
//! (Revision 3 of `docs/design/injectable-modules.md`).
//!
//! Loaded once at startup from `modules_dir`: every `*.js` / `*.mjs` file becomes a
//! module whose specifier is its relative path without the extension
//! (`acme/pricing.mjs` → `acme/pricing`). The map is immutable at runtime, and resolution
//! is a pure in-memory `HashMap` lookup wired into `QuickJS` via a [`Resolver`] + [`Loader`]
//! — there is **no filesystem access at import time**, so a script can `import` only
//! registered modules, never an arbitrary path.

use std::collections::HashMap;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::Declared;
use rquickjs::{Ctx, Error as JsError, Module, Result as JsResult};

/// Immutable specifier → module-source map, loaded at startup.
#[derive(Debug, Default)]
pub(crate) struct ModuleRegistry {
    /// Registered modules: specifier (relative path, no extension) → ESM source.
    modules: HashMap<String, Arc<str>>,
}

impl ModuleRegistry {
    /// Loads every `*.js` / `*.mjs` file under `dir`, recursively. Each module is validated
    /// against `max_size` at load — a too-large module is a startup error, never a runtime
    /// surprise.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory or a file can't be read, a module exceeds
    /// `max_size`, or a module path isn't valid UTF-8.
    pub(crate) fn load(dir: &Path, max_size: usize) -> Result<Self, Box<dyn Error + Send + Sync>> {
        let mut modules = HashMap::new();
        let mut pending: Vec<PathBuf> = vec![dir.to_path_buf()];
        while let Some(current) = pending.pop() {
            for entry in fs::read_dir(&current)? {
                let path = entry?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path
                    .extension()
                    .is_some_and(|ext| ext == "js" || ext == "mjs")
                {
                    let source = fs::read_to_string(&path)?;
                    if source.len() > max_size {
                        return Err(format!(
                            "registered module {} is {} bytes (max_script_size is {max_size})",
                            path.display(),
                            source.len(),
                        )
                        .into());
                    }
                    drop(modules.insert(derive_key(dir, &path)?, Arc::from(source)));
                }
            }
        }
        Ok(Self { modules })
    }

    /// The source for a registered specifier, if any.
    fn source(&self, specifier: &str) -> Option<Arc<str>> {
        self.modules.get(specifier).map(Arc::clone)
    }

    /// Whether a specifier is registered (pure map lookup — `../` has no meaning).
    fn contains(&self, specifier: &str) -> bool {
        self.modules.contains_key(specifier)
    }

    /// Number of registered modules.
    pub(crate) fn count(&self) -> usize {
        self.modules.len()
    }
}

/// Derives the registry specifier for `path`: its path relative to `root`, `/`-separated,
/// without the extension.
fn derive_key(root: &Path, path: &Path) -> Result<String, Box<dyn Error + Send + Sync>> {
    let relative = path.strip_prefix(root)?.with_extension("");
    let parts = relative
        .components()
        .map(|comp| {
            comp.as_os_str()
                .to_str()
                .ok_or("module path is not valid UTF-8")
        })
        .collect::<Result<Vec<&str>, &str>>()?;
    Ok(parts.join("/"))
}

/// `QuickJS` module resolver backed by the registry: a bare specifier resolves to itself
/// **iff** it is registered; anything else (relative `./`, unknown names, absolute paths)
/// fails. This is the security property — a script reaches only registered modules.
pub(crate) struct RegistryResolver(pub(crate) Arc<ModuleRegistry>);

impl Resolver for RegistryResolver {
    fn resolve<'js>(
        &mut self,
        _ctx: &Ctx<'js>,
        base: &str,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> JsResult<String> {
        if self.0.contains(name) {
            Ok(name.to_owned())
        } else {
            Err(JsError::new_resolving(base.to_owned(), name.to_owned()))
        }
    }
}

/// `QuickJS` module loader backed by the registry: declares the registered source as an
/// ES module. Never touches the filesystem — the source is already in memory.
pub(crate) struct RegistryLoader(pub(crate) Arc<ModuleRegistry>);

impl Loader for RegistryLoader {
    fn load<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        name: &str,
        _attributes: Option<ImportAttributes<'js>>,
    ) -> JsResult<Module<'js, Declared>> {
        let source = self
            .0
            .source(name)
            .ok_or_else(|| JsError::new_loading(name.to_owned()))?;
        Module::declare(ctx.clone(), name, source.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    //! Registry load/lookup + the load-bearing "pure in-memory map, no traversal" property.

    use super::ModuleRegistry;
    use std::env;
    use std::error::Error;
    use std::fs;
    use std::path::PathBuf;

    /// Boxed error alias to keep test signatures short.
    type TestResult = Result<(), Box<dyn Error + Send + Sync>>;

    /// Creates a unique temp fixture dir with a `.mjs` and a nested `.js` module.
    fn fixture_dir(tag: &str) -> Result<PathBuf, Box<dyn Error + Send + Sync>> {
        let dir = env::temp_dir().join(format!("jsbox-modules-test-{tag}"));
        let nested = dir.join("acme");
        fs::create_dir_all(&nested)?;
        fs::write(dir.join("util.mjs"), "export const x = 1;")?;
        fs::write(
            nested.join("pricing.js"),
            "export function quote(n){ return n; }",
        )?;
        fs::write(dir.join("notes.txt"), "not a module")?;
        Ok(dir)
    }

    /// `.js` and `.mjs` load under `/`-separated, extensionless specifiers; other files don't.
    #[test]
    fn loads_js_and_mjs() -> TestResult {
        let dir = fixture_dir("load")?;
        let registry = ModuleRegistry::load(&dir, 1024)?;
        assert_eq!(registry.count(), 2, "the .mjs and .js both load");
        assert!(registry.contains("util"), ".mjs specifier resolves");
        assert!(
            registry.contains("acme/pricing"),
            "nested .js specifier resolves"
        );
        assert!(!registry.contains("notes"), "non-module files are ignored");
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// Traversal-shaped specifiers never resolve — lookup is a `HashMap` hit, so `../` has
    /// no filesystem meaning and a script can never `import` outside `modules_dir`.
    #[test]
    fn traversal_specifiers_never_resolve() -> TestResult {
        let dir = fixture_dir("traversal")?;
        let registry = ModuleRegistry::load(&dir, 1024)?;
        for evil in [
            "../util",
            "../../etc/passwd",
            "/etc/passwd",
            "./util",
            "acme/../util",
            "util.mjs",
        ] {
            assert!(!registry.contains(evil), "must not resolve: {evil}");
        }
        assert!(
            registry.contains("util"),
            "the plain specifier still resolves"
        );
        fs::remove_dir_all(&dir)?;
        Ok(())
    }

    /// A module over `max_size` fails the whole load (startup error, never a runtime miss).
    #[test]
    fn oversized_module_fails_load() -> TestResult {
        let dir = fixture_dir("oversized")?;
        assert!(
            ModuleRegistry::load(&dir, 8).is_err(),
            "oversized module fails load"
        );
        fs::remove_dir_all(&dir)?;
        Ok(())
    }
}
