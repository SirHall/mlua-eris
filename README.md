# mlua-eris

Safe [mlua](https://github.com/mlua-rs/mlua) wrapper around [Eris](https://github.com/fnuecke/eris) persistence. Serialize a mid-execution Lua coroutine, closure, or any other Lua value to bytes — and restore it into a fresh Lua state later. Designed for save-game systems where in-flight scripted entities must persist across program restarts.

## What this enables

```rust
use mlua_eris::ErisLua;

// Set up a Lua state and create a suspended coroutine.
let eris_a = ErisLua::new()?;
eris_a.lua().load(r#"
    co = coroutine.create(function()
        coroutine.yield(1); coroutine.yield(2); coroutine.yield(3)
    end)
    coroutine.resume(co)  -- yields 1, then suspends
"#).call::<()>(())?;

// Serialize the suspended coroutine to bytes.
let co: mlua::Value = eris_a.lua().globals().get("co")?;
let blob: Vec<u8> = eris_a.persist(co)?;

// ... write `blob` to a save file, transmit over network, etc. ...

// Open a totally fresh Lua state and restore.
let eris_b = ErisLua::new()?;
let restored = eris_b.unpersist(&blob)?;
eris_b.lua().globals().set("co", restored)?;

// Resume in the new state — picks up exactly where the original suspended.
let next: i64 = eris_b.lua()
    .load("local _, v = coroutine.resume(co); return v")
    .call(())?;
assert_eq!(next, 2);  // would be 1 if the coroutine had restarted
```

This works for closures with upvalues, tables with cycles, deeply-nested coroutine call stacks, and any combination thereof.

## Why this exists

Most Rust scripting languages (Rhai, Koto, Wren, even Piccolo) cannot serialize a mid-execution coroutine. Lua has been able to do this since 2008 via the [Pluto](http://lua-users.org/wiki/PlutoLibrary) and later [Eris](https://github.com/fnuecke/eris) C libraries — but those were never wrapped for Rust. This crate plus `lua-eris-sys` provide the missing layer.

Used in production by [OpenComputers](https://github.com/MightyPirates/OpenComputers) (Minecraft mod) for ~10 years to persist running in-game computers across world saves. Battle-tested for the exact use case of "scripted entity in mid-execution must survive a save/load cycle."

## Architecture

```
┌──────────────────────────────────────────────┐
│  Your code:  ErisLua::persist / unpersist    │ ← safe Rust API
├──────────────────────────────────────────────┤
│  mlua 0.11 (with mlua-sys "external")        │ ← stock crates,                                                    no fork
├──────────────────────────────────────────────┤
│  lua-eris-sys (Lua 5.3.5 + eris.c bundled)   │ ← static lib
└──────────────────────────────────────────────┘
```

mlua's standard high-level API (`Lua::load`, `Lua::create_function`, etc.) all work normally; `mlua-eris` only adds persistence on top.

## Perms tables — the one concept you have to learn

Eris cannot serialize light C functions (Rust callbacks registered via `mlua::Lua::create_function`), userdata, or other "opaque" host values. Instead, you supply a **perms table** — a `value → string_key` map. When persistence encounters one of those mapped values, it writes the key into the blob; on restore, the key is looked up to find the host-side reference (typically a freshly-registered function in the new Lua state).

`ErisLua::new()` builds a default perms table containing every Lua standard library function (so coroutines that call `coroutine.yield`, `string.match`, etc. round-trip correctly). To register YOUR OWN Rust functions:

```rust
let mut eris = ErisLua::new()?;
let host_fn = eris.lua().create_function(|_, x: i64| Ok(x * 2))?;
eris.lua().globals().set("double", host_fn.clone())?;
eris.register_perm("host.double", host_fn)?;
// ... now any closure that captures `double` can be persisted.
```

**The string keys are part of your save file format.** Renaming a Rust function but keeping the same key is fine. Changing a key breaks every existing save that references the old key. Treat keys like database column names.

## Status

`v0.1.0` — Phase 1 complete. The architecture is validated end-to-end; not yet polished for general consumption.

- ✅ Mid-execution coroutine persistence works
- ✅ Closures with upvalues round-trip
- ✅ Custom Rust function perms work
- ✅ End-to-end spike with a real-world script (spaceship's `transponder.rhai` ported to Lua) passes
- ⚠️ API may shift before 1.0
- ⚠️ Documentation is sparse beyond the rustdoc
- ⚠️ Only macOS/Linux tested (Windows would need a build.rs tweak in `lua-eris-sys`)

## License

MIT.

Underlying dependencies: Lua and Eris are both MIT-licensed; mlua and mlua-sys are MIT-licensed.
