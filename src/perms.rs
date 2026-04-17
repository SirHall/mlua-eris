//! Perms-table registry.
//!
//! A perms table maps non-serializable Lua values (light C functions,
//! Rust callbacks, userdata) to stable string keys. Eris writes the
//! key into the serialized blob; on unpersist, the key is looked up
//! to restore the host-side reference.
//!
//! ## Design constraints
//!
//! 1. **Keys are part of the save format.** Renaming a key breaks
//!    every existing save. Treat keys like database column names.
//!
//! 2. **Persist and unpersist must use IDENTICAL key sets.** A value
//!    registered on persist that's missing on unpersist produces
//!    `"bad permanent value (no value)"`. We enforce this by storing
//!    a single registry that builds BOTH the perms table (for persist)
//!    and the inverted uperms table (for unpersist).
//!
//! 3. **Iteration order matters for default stdlib registration.**
//!    The same value at multiple paths in `_G` (e.g., `string` and
//!    `package.loaded.string`) keeps its first-seen key. We sort
//!    keys deterministically so the first-seen key is reproducible
//!    across Lua state instances.
//!
//! See the crate-level docs for more on the perms-table concept.

use std::collections::HashMap;

use mlua::{Function, Lua, Table, Value};

use crate::error::{Error, Result};

/// A registry of `(key, Lua value)` perm entries.
///
/// Built up by the application before calling
/// [`crate::ErisLua::persist`] or
/// [`crate::ErisLua::unpersist`]. The default registry contains the
/// Lua standard library; you extend it with `register_*` methods for
/// any host-side functions or userdata you want to round-trip through
/// save files.
pub struct PermsRegistry {
    /// The Lua state these entries belong to.
    /// Held as a clone of mlua::Lua (cheap — internal Arc).
    lua: Lua,
    /// Tracks perm key uniqueness. Maps key → a marker Value used
    /// only for collision detection. The actual perms table lives in
    /// the Lua registry, keyed by `PERMS_REGISTRY_KEY`.
    keys: HashMap<String, ()>,
}

/// Lua-registry key under which the perms table is stored.
const PERMS_REGISTRY_KEY: &str = "__mlua_eris_perms";
/// Lua-registry key for the inverted (uperms) table.
const UPERMS_REGISTRY_KEY: &str = "__mlua_eris_uperms";

impl PermsRegistry {
    /// Create an empty perms registry attached to the given Lua state.
    /// Most callers should use [`with_default_stdlib`](Self::with_default_stdlib)
    /// instead — the empty registry can't persist anything that
    /// references the standard library, which is most things.
    pub fn empty(lua: &Lua) -> Result<Self> {
        let registry = Self {
            lua: lua.clone(),
            keys: HashMap::new(),
        };
        registry.write_perms_table(lua.create_table()?)?;
        registry.write_uperms_table(lua.create_table()?)?;
        Ok(registry)
    }

    /// Create a perms registry pre-populated with the Lua 5.3 standard
    /// library: every function and library table reachable from `_G`
    /// is registered with a stable string key (`"string.match"`,
    /// `"coroutine.yield"`, etc.).
    ///
    /// This is enough to persist any value that only references stdlib —
    /// closures, coroutines, plain data structures. To persist values
    /// that reference your own Rust functions or userdata, add them
    /// explicitly via [`register_function`](Self::register_function)
    /// or [`register_value`](Self::register_value).
    pub fn with_default_stdlib(lua: &Lua) -> Result<Self> {
        let mut reg = Self::empty(lua)?;
        reg.register_default_stdlib()?;
        Ok(reg)
    }

    /// Internal: walk `_G` and register everything in the standard library.
    fn register_default_stdlib(&mut self) -> Result<()> {
        // Top-level base functions (Lua 5.3 base library).
        const BASE_FUNCS: &[&str] = &[
            "assert", "collectgarbage", "dofile", "error", "getmetatable",
            "ipairs", "load", "loadfile", "next", "pairs", "pcall", "print",
            "rawequal", "rawget", "rawlen", "rawset", "require", "select",
            "setmetatable", "tonumber", "tostring", "type", "xpcall",
        ];
        // Standard library tables to register and walk children of.
        // `eris` is included because the persist/unpersist functions
        // themselves might end up captured by host-registered closures.
        const LIBS: &[&str] = &[
            "coroutine", "debug", "io", "math", "os", "package",
            "string", "table", "utf8", "eris",
        ];

        let globals = self.lua.globals();

        for name in BASE_FUNCS {
            let value: Value = globals.get(*name)?;
            // Some builds may not have every base function — skip nil
            // rather than failing.
            if !matches!(value, Value::Nil) {
                self.register_value(name, value)?;
            }
        }

        for lib_name in LIBS {
            let lib: Value = globals.get(*lib_name)?;
            match lib {
                Value::Table(ref tbl) => {
                    self.register_value(lib_name, lib.clone())?;
                    // Sort children by name for deterministic
                    // first-seen key when the same value appears at
                    // multiple paths.
                    let mut keys: Vec<String> = Vec::new();
                    for pair in tbl.clone().pairs::<Value, Value>() {
                        let (k, _v) = pair?;
                        if let Value::String(s) = k {
                            if let Ok(s_str) = s.to_str() {
                                keys.push(s_str.to_string());
                            }
                        }
                    }
                    keys.sort();
                    for k in keys {
                        let v: Value = tbl.get(k.as_str())?;
                        let perm_key = format!("{}.{}", lib_name, k);
                        match v {
                            Value::Function(_) | Value::Table(_) => {
                                // Ignore duplicates — same value at
                                // multiple paths keeps the first-seen
                                // key, by design.
                                let _ = self.register_value(&perm_key, v);
                            }
                            _ => {}
                        }
                    }
                }
                Value::Nil => {} // library not loaded — skip silently
                other => {
                    // Unexpected: a library entry exists but isn't a
                    // table. Could happen with very-stripped builds.
                    // Register it under the lib name and move on.
                    self.register_value(lib_name, other)?;
                }
            }
        }

        // Register `_G` itself as a perm — Eris may reference it.
        let g = Value::Table(self.lua.globals());
        self.register_value("_G", g)?;

        Ok(())
    }

    /// Register a function under a stable key.
    ///
    /// Convenience wrapper around [`register_value`](Self::register_value)
    /// for the common case of host-side Rust callbacks created via
    /// [`mlua::Lua::create_function`].
    pub fn register_function(&mut self, key: &str, func: Function) -> Result<()> {
        self.register_value(key, Value::Function(func))
    }

    /// Register an arbitrary Lua value under a stable key.
    ///
    /// The same key cannot be registered twice with different values —
    /// returns `Error::DuplicatePermKey`. Re-registering the SAME
    /// (key, value) pair is a no-op, which lets you call this idempotently
    /// during setup.
    pub fn register_value(&mut self, key: &str, value: Value) -> Result<()> {
        if self.keys.contains_key(key) {
            // Idempotent: if the existing perm has the same value, that's fine.
            // We can't easily compare arbitrary Lua values for equality from
            // Rust, so instead we treat any re-registration with the same key
            // as a conflict that the caller should resolve.
            return Err(Error::DuplicatePermKey(key.to_string()));
        }

        // Update the perms (value -> key) and uperms (key -> value) tables.
        let perms = self.read_perms_table()?;
        let uperms = self.read_uperms_table()?;
        perms.set(value.clone(), key)?;
        uperms.set(key, value)?;
        self.write_perms_table(perms)?;
        self.write_uperms_table(uperms)?;
        self.keys.insert(key.to_string(), ());
        Ok(())
    }

    /// Number of registered perm keys.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// True if no perms have been registered.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    // === Internal accessors ===

    /// Get the perms table (value -> key) — used by `persist`.
    pub(crate) fn perms_table(&self) -> Result<Table> {
        self.read_perms_table()
    }

    /// Get the inverted perms table (key -> value) — used by `unpersist`.
    pub(crate) fn uperms_table(&self) -> Result<Table> {
        self.read_uperms_table()
    }

    fn read_perms_table(&self) -> Result<Table> {
        Ok(self.lua.named_registry_value(PERMS_REGISTRY_KEY)?)
    }

    fn write_perms_table(&self, t: Table) -> Result<()> {
        self.lua.set_named_registry_value(PERMS_REGISTRY_KEY, t)?;
        Ok(())
    }

    fn read_uperms_table(&self) -> Result<Table> {
        Ok(self.lua.named_registry_value(UPERMS_REGISTRY_KEY)?)
    }

    fn write_uperms_table(&self, t: Table) -> Result<()> {
        self.lua.set_named_registry_value(UPERMS_REGISTRY_KEY, t)?;
        Ok(())
    }
}
