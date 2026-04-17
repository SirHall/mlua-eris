//! # mlua-eris
//!
//! Safe wrapper around [Eris](https://github.com/fnuecke/eris)
//! persistence on top of [mlua](https://github.com/mlua-rs/mlua).
//! Lets you serialize a mid-execution Lua coroutine (including its
//! full call stack), closures with upvalues, tables with cycles, and
//! arbitrary other Lua values to bytes — and restore them into a
//! fresh Lua state later. The intended use case is save-game systems
//! where in-flight scripted entities (NPC behaviours, player ship
//! computers, etc.) must persist across program restarts.
//!
//! ## Quick start
//!
//! ```
//! use mlua_eris::ErisLua;
//!
//! // Open a fresh Lua state with Eris loaded.
//! let eris_a = ErisLua::new().unwrap();
//! let lua_a = eris_a.lua();
//!
//! // Run some code that creates a suspended coroutine.
//! lua_a.load(r#"
//!     co = coroutine.create(function()
//!         coroutine.yield(1)
//!         coroutine.yield(2)
//!     end)
//!     coroutine.resume(co)
//! "#).call::<()>(()).unwrap();
//!
//! // Serialize the suspended coroutine to bytes.
//! let co: mlua::Value = lua_a.globals().get("co").unwrap();
//! let blob = eris_a.persist(co).unwrap();
//!
//! // ... store `blob` somewhere (disk, network, save file) ...
//!
//! // Open a fresh Lua state and restore.
//! let eris_b = ErisLua::new().unwrap();
//! let restored: mlua::Value = eris_b.unpersist(&blob).unwrap();
//! eris_b.lua().globals().set("co", restored).unwrap();
//!
//! // Resume — yields the next value from where the original suspended.
//! let next: i64 = eris_b.lua()
//!     .load("local _, v = coroutine.resume(co); return v")
//!     .call(()).unwrap();
//! assert_eq!(next, 2);
//! ```
//!
//! ## Perms tables: the main concept you have to learn
//!
//! Eris cannot serialize C functions, native userdata, or other
//! "opaque" host values. Instead, you supply a **perms table** — a
//! `value → string_key` map. When serialization encounters one of
//! those mapped values, it writes the string key instead of trying
//! to walk the value's contents. On deserialization, the inverse
//! map (key → value) restores the host-side reference.
//!
//! `ErisLua` builds a default perms table containing the Lua
//! standard library (so `coroutine.yield`, `string.match`, etc. round-
//! trip correctly even though they're light C functions). To register
//! YOUR OWN Rust functions or userdata for persistence, use
//! [`ErisLua::register_perm`] before the first `persist` call.
//!
//! ### Stable keys are load-bearing
//!
//! The string key you use for `register_perm` is part of your save
//! file format. Renaming a Rust function but keeping the same key in
//! perms is fine. CHANGING the key breaks every existing save that
//! references the old key. Treat keys like database column names.

#![deny(missing_docs)]

mod error;
mod perms;
mod state;

pub use error::{Error, Result};
pub use perms::PermsRegistry;
pub use state::ErisLua;

// Re-export the underlying mlua so callers don't need to add it as
// a separate dep with potentially-mismatched feature flags.
pub use mlua;

// Force-link lua-eris-sys's static archive. Without this `use`,
// cargo treats lua-eris-sys as rmeta-only and drops its
// `cargo:rustc-link-lib=static=lua-eris` directive — leaving the
// linker unable to find `_luaopen_coroutine` and friends.
#[allow(unused_imports)]
use lua_eris_sys::eris_persist as _force_link;
