//! Public API tests for mlua-eris.

use mlua_eris::{ErisLua, Error};

fn run(lua: &mlua::Lua, code: &str) {
    lua.load(code).call::<()>(()).expect("lua call failed");
}

// ============================================================
// Basic state setup
// ============================================================

#[test]
fn fresh_state_has_eris_global() {
    let eris = ErisLua::new().expect("ErisLua::new");
    let has: bool = eris.lua().load("return type(eris) == 'table'").call(()).unwrap();
    assert!(has, "global `eris` table should be loaded");
}

#[test]
fn perms_registry_starts_with_stdlib() {
    let eris = ErisLua::new().unwrap();
    // ~25 base funcs + ~10 libs * ~20 functions = a few hundred entries.
    // Just check we have a non-trivial baseline.
    assert!(
        eris.perms().len() > 50,
        "default stdlib perms should have many entries, got {}",
        eris.perms().len()
    );
}

// ============================================================
// Basic round-trips (no custom perms needed — only stdlib)
// ============================================================

#[test]
fn round_trip_an_integer() {
    let eris = ErisLua::new().unwrap();
    let v: mlua::Value = mlua::Value::Integer(42);
    let blob = eris.persist(v).unwrap();

    let eris2 = ErisLua::new().unwrap();
    let restored: i64 = eris2.unpersist(&blob).unwrap().as_integer().unwrap();
    assert_eq!(restored, 42);
}

#[test]
fn round_trip_a_table() {
    let eris = ErisLua::new().unwrap();
    run(eris.lua(), r#"t = { name = "hello", n = 7 }"#);
    let t: mlua::Value = eris.lua().globals().get("t").unwrap();
    let blob = eris.persist(t).unwrap();

    let eris2 = ErisLua::new().unwrap();
    let restored = eris2.unpersist(&blob).unwrap();
    eris2.lua().globals().set("t", restored).unwrap();
    run(
        eris2.lua(),
        r#"
        assert(t.name == "hello")
        assert(t.n == 7)
        "#,
    );
}

#[test]
fn round_trip_a_closure_with_upvalue() {
    let eris = ErisLua::new().unwrap();
    run(
        eris.lua(),
        r#"
        local function make_counter(start)
            local n = start
            return function() n = n + 1; return n end
        end
        counter = make_counter(100)
        assert(counter() == 101)
        assert(counter() == 102)
        "#,
    );
    let c: mlua::Value = eris.lua().globals().get("counter").unwrap();
    let blob = eris.persist(c).unwrap();

    let eris2 = ErisLua::new().unwrap();
    eris2.lua().globals().set("counter", eris2.unpersist(&blob).unwrap()).unwrap();
    run(
        eris2.lua(),
        r#"
        assert(counter() == 103)
        assert(counter() == 104)
        "#,
    );
}

#[test]
fn round_trip_a_suspended_coroutine_with_deep_yield() {
    // The load-bearing test — proves the entire migration is justified.
    let eris = ErisLua::new().unwrap();
    run(
        eris.lua(),
        r#"
        local function inner(v) coroutine.yield(v) end
        local function middle(v) inner(v) end
        co = coroutine.create(function()
            middle(10); middle(20); middle(30)
        end)
        local _, v1 = coroutine.resume(co); assert(v1 == 10)
        local _, v2 = coroutine.resume(co); assert(v2 == 20)
        assert(coroutine.status(co) == "suspended")
        "#,
    );

    let co: mlua::Value = eris.lua().globals().get("co").unwrap();
    let blob = eris.persist(co).unwrap();
    eprintln!("suspended coroutine: {} bytes", blob.len());

    let eris2 = ErisLua::new().unwrap();
    eris2.lua().globals().set("co", eris2.unpersist(&blob).unwrap()).unwrap();
    run(
        eris2.lua(),
        r#"
        assert(coroutine.status(co) == "suspended")
        local _, v = coroutine.resume(co)
        assert(v == 30, "expected 30, got " .. tostring(v))
        coroutine.resume(co)  -- finalize
        assert(coroutine.status(co) == "dead")
        "#,
    );
}

// ============================================================
// Custom perm registration — the spaceship use case
// ============================================================

#[test]
fn rust_function_as_perm_round_trips_in_a_closure() {
    // Simulates registering a host-side function (like spaceship's
    // `screen_write`) as a perm so closures that reference it can be
    // persisted.
    let mut eris = ErisLua::new().unwrap();

    // Register a Rust function under a stable key.
    let host_fn = eris
        .lua()
        .create_function(|_, x: i64| Ok(x * 100))
        .unwrap();
    eris.lua().globals().set("host_multiply", host_fn.clone()).unwrap();
    eris.register_perm("host.multiply", host_fn).unwrap();

    // Build a closure that captures host_multiply via upvalue.
    run(
        eris.lua(),
        r#"
        local fn = host_multiply  -- pull into upvalue
        captured = function(x) return fn(x) + 1 end
        assert(captured(2) == 201)
        "#,
    );

    // Persist the closure. Without perms registration this would fail
    // with "attempt to persist a light C function".
    let c: mlua::Value = eris.lua().globals().get("captured").unwrap();
    let blob = eris.persist(c).unwrap();

    // Restore in a fresh state — must register the SAME perm key →
    // a new instance of the function (could be a different Rust fn,
    // as long as the signature matches what callers expect).
    let mut eris2 = ErisLua::new().unwrap();
    let host_fn2 = eris2
        .lua()
        .create_function(|_, x: i64| Ok(x * 100))
        .unwrap();
    eris2.register_perm("host.multiply", host_fn2).unwrap();

    let restored = eris2.unpersist(&blob).unwrap();
    eris2.lua().globals().set("captured", restored).unwrap();
    let result: i64 = eris2.lua().load("return captured(3)").call(()).unwrap();
    assert_eq!(result, 301, "restored closure should call host fn");
}

#[test]
fn duplicate_perm_key_is_an_error() {
    let mut eris = ErisLua::new().unwrap();
    let f1 = eris.lua().create_function(|_, ()| Ok(())).unwrap();
    let f2 = eris.lua().create_function(|_, ()| Ok(())).unwrap();

    eris.register_perm("dup.key", f1).unwrap();
    let result = eris.register_perm("dup.key", f2);
    assert!(matches!(result, Err(Error::DuplicatePermKey(_))));
}

// ============================================================
// Error reporting
// ============================================================

#[test]
fn persist_an_unregistered_c_function_returns_error() {
    // A registered Rust function NOT in perms should produce
    // PersistFailed, not a crash.
    let eris = ErisLua::new().unwrap();
    let host_fn = eris
        .lua()
        .create_function(|_, ()| Ok("hello"))
        .unwrap();
    eris.lua().globals().set("naked_fn", host_fn).unwrap();

    // Persist the function directly. Wrapping a plain C function in
    // an mlua Function still goes through Eris's "is this a light C
    // function?" check.
    let v: mlua::Value = eris.lua().globals().get("naked_fn").unwrap();
    let result = eris.persist(v);
    assert!(matches!(result, Err(Error::PersistFailed(_))));
    eprintln!("got expected error: {:?}", result.err());
}

#[test]
fn unpersist_garbage_returns_error() {
    let eris = ErisLua::new().unwrap();
    let result = eris.unpersist(b"this is not a valid eris blob");
    assert!(matches!(result, Err(Error::UnpersistFailed(_))));
}
