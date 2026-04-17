//! [`ErisLua`] — an `mlua::Lua` paired with an Eris perms registry.

use mlua::{Function, Lua, Value};

use crate::error::{Error, Result};
use crate::perms::PermsRegistry;

extern "C-unwind" {
    /// Defined in eris.c (bundled in `lua-eris-sys`). Pushes the Eris
    /// module table onto the stack when called as a Lua C function.
    fn luaopen_eris(L: *mut mlua_sys::lua_State) -> std::os::raw::c_int;
}

/// A Lua state with the Eris persistence library loaded and a perms
/// registry attached.
///
/// Construct with [`ErisLua::new`] (default, safe stdlib subset),
/// [`ErisLua::unsafe_new`] (full stdlib including io/os/debug), or
/// [`ErisLua::from_lua`] to attach Eris to an existing `mlua::Lua`.
///
/// All three start with a perms registry pre-populated with the Lua
/// standard library; extend it via the methods on
/// [`PermsRegistry`] (accessed through [`Self::perms_mut`]) before
/// calling [`Self::persist`] or [`Self::unpersist`].
pub struct ErisLua {
    lua: Lua,
    perms: PermsRegistry,
}

impl ErisLua {
    /// Create a fresh Lua state with the safe stdlib subset (no
    /// `io`, `os`, `debug`) plus Eris loaded as global `eris`.
    /// Equivalent to `mlua::Lua::new()` + Eris loading + default
    /// perms registration.
    pub fn new() -> Result<Self> {
        Self::from_lua(Lua::new())
    }

    /// Create a fresh Lua state with the FULL stdlib (including io,
    /// os, debug — potentially exploitable in adversarial sandboxes)
    /// plus Eris loaded as global `eris`.
    ///
    /// # Safety
    ///
    /// Same caveat as `mlua::Lua::unsafe_new` — host-untrusted code
    /// running in this state can read/write the host filesystem,
    /// inspect debug info, etc. Use only for trusted scripts.
    pub unsafe fn unsafe_new() -> Result<Self> {
        Self::from_lua(Lua::unsafe_new())
    }

    /// Attach Eris and a default perms registry to an existing
    /// `mlua::Lua` state. Use this if you've already configured the
    /// state with custom modules, allocators, hooks, etc.
    pub fn from_lua(lua: Lua) -> Result<Self> {
        unsafe {
            let opener = lua.create_c_function(luaopen_eris)?;
            let eris_table: mlua::Table = opener.call(())?;
            lua.globals().set("eris", eris_table)?;
        }
        let perms = PermsRegistry::with_default_stdlib(&lua)?;
        Ok(Self { lua, perms })
    }

    /// Borrow the underlying `mlua::Lua` state. Use this for any
    /// regular mlua API call — running scripts, registering
    /// functions, building values, etc.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Borrow the perms registry — primarily for inspecting size /
    /// emptiness. To register new perms, use [`Self::perms_mut`].
    pub fn perms(&self) -> &PermsRegistry {
        &self.perms
    }

    /// Mutably borrow the perms registry to register host-side
    /// callbacks and userdata. Call this BEFORE the first persist
    /// involving values that reference those callbacks.
    pub fn perms_mut(&mut self) -> &mut PermsRegistry {
        &mut self.perms
    }

    /// Register a host Rust function under a stable perm key.
    /// Convenience wrapper around `perms_mut().register_function(...)`.
    pub fn register_perm(&mut self, key: &str, func: Function) -> Result<()> {
        self.perms.register_function(key, func)
    }

    /// Serialize a Lua value (typically a suspended coroutine, a
    /// closure with upvalues, or a table containing application state)
    /// to bytes via Eris.
    ///
    /// Internally calls Lua's `eris.persist(perms, value)` — this runs
    /// inside Lua's protected execution, so any errors (light C
    /// function not in perms, etc.) are caught and returned as
    /// `Error::PersistFailed` rather than aborting the process.
    pub fn persist(&self, value: Value) -> Result<Vec<u8>> {
        let eris: mlua::Table = self.lua.globals().get("eris")?;
        let persist_fn: Function = eris.get("persist")?;
        let perms = self.perms.perms_table()?;
        let result: mlua::String = persist_fn
            .call((perms, value))
            .map_err(|e| Error::PersistFailed(e.to_string()))?;
        Ok(result.as_bytes().to_vec())
    }

    /// Restore a value from bytes produced by [`Self::persist`] (in
    /// this or any other Lua state with the same perms registry).
    ///
    /// The returned `Value` is a fresh Lua reference in this state.
    /// The original Lua state's reference is irrelevant — the entire
    /// graph is reconstructed from the byte stream.
    pub fn unpersist(&self, blob: &[u8]) -> Result<Value> {
        let eris: mlua::Table = self.lua.globals().get("eris")?;
        let unpersist_fn: Function = eris.get("unpersist")?;
        let uperms = self.perms.uperms_table()?;
        let blob_str = self.lua.create_string(blob)?;
        let result: Value = unpersist_fn
            .call((uperms, blob_str))
            .map_err(|e| Error::UnpersistFailed(e.to_string()))?;
        Ok(result)
    }
}
