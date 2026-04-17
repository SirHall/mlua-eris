//! Phase 0 / Phase 1 spike: port spaceship's `transponder.rhai` service
//! to Lua and prove the entire pattern works through `mlua-eris`.
//!
//! Original Rhai: assets/volumes/openos/os/services/transponder.rhai
//! Behaviour: broadcasts a SQUAK presence message on frequency 950
//! every 300 kernel ticks, identifying the ship.
//!
//! What this test validates:
//! 1. The script can be ported to Lua line-for-line with idiomatic
//!    Lua (no AST transform, no co_yield → coroutine.yield rewrite,
//!    no closure-capture hack).
//! 2. Host-stub functions (fs.exists, fs.cat, squak.send) registered
//!    as Lua tables work as expected.
//! 3. `coroutine.yield` from inside the loop body suspends the
//!    coroutine — the kernel-style scheduler resumes it once per tick.
//! 4. The suspended service can be persisted mid-execution.
//! 5. After restore in a fresh Lua state (with the same host stubs
//!    re-registered), resuming the service continues from where it
//!    left off — same counter value, same ident, same loop iteration.
//!
//! If this works, the cost of porting the rest of spaceship's Rhai
//! scripts to Lua is bounded: the largest single concern (mid-flight
//! persistence) is solved. The remaining work is mechanical syntax
//! translation.

use mlua_eris::{mlua, ErisLua};
use std::sync::{Arc, Mutex};

/// Recording of a SQUAK broadcast by the transponder.
#[derive(Debug, Clone, PartialEq)]
struct Broadcast {
    freq: i64,
    ident: String,
    stance: String,
}

/// Shared state visible to both Rust and Lua: every call to
/// `squak.send(freq, msg)` appends a Broadcast here. Tests inspect
/// this to verify what the script produced.
type BroadcastLog = Arc<Mutex<Vec<Broadcast>>>;

/// Set up host-side Rust stubs in the given Lua state:
/// - `fs.exists(path)` — returns true iff path is "/etc/hostname"
/// - `fs.cat(path)` — returns "HMS Spike" for "/etc/hostname"
/// - `squak.presence(type, ident, stance)` — builds a message table
/// - `squak.send(freq, msg)` — records to the shared log
///
/// All four are registered as PERMS so they survive persist/unpersist.
/// Each function gets a stable string key. The same keys must be used
/// when re-registering on the unpersist side.
fn setup_host_stubs(eris: &mut ErisLua, log: BroadcastLog) -> mlua::Result<()> {
    let lua = eris.lua().clone();

    // --- fs table ---
    let fs = lua.create_table()?;
    let fs_exists = lua.create_function(|_, path: String| {
        Ok(path == "/etc/hostname")
    })?;
    let fs_cat = lua.create_function(|_, path: String| {
        if path == "/etc/hostname" {
            Ok(Some("HMS Spike".to_string()))
        } else {
            Ok(None)
        }
    })?;
    fs.set("exists", fs_exists.clone())?;
    fs.set("cat", fs_cat.clone())?;
    lua.globals().set("fs", fs)?;
    eris.register_perm("host.fs.exists", fs_exists).unwrap();
    eris.register_perm("host.fs.cat", fs_cat).unwrap();

    // --- squak table ---
    let squak = lua.create_table()?;
    let squak_presence = lua.create_function(
        |lua, (msg_type, ident, stance): (String, String, String)| {
            let t = lua.create_table()?;
            t.set("type", msg_type)?;
            t.set("ident", ident)?;
            t.set("stance", stance)?;
            Ok(t)
        },
    )?;
    let log_clone = log.clone();
    let squak_send = lua.create_function(
        move |_, (freq, msg): (i64, mlua::Table)| {
            let ident: String = msg.get("ident")?;
            let stance: String = msg.get("stance")?;
            log_clone.lock().unwrap().push(Broadcast { freq, ident, stance });
            Ok(())
        },
    )?;
    squak.set("presence", squak_presence.clone())?;
    squak.set("send", squak_send.clone())?;
    lua.globals().set("squak", squak)?;
    eris.register_perm("host.squak.presence", squak_presence).unwrap();
    eris.register_perm("host.squak.send", squak_send).unwrap();

    Ok(())
}

/// The transponder service ported to Lua. Compare to:
/// assets/volumes/openos/os/services/transponder.rhai
///
/// Note: NO yield-at-top trick is needed (Rhai needs that because its
/// "first step on compile engine" can't see the OS stdlib yet — Lua
/// has no such constraint). The function runs straight through.
const TRANSPONDER_LUA: &str = r#"
local _transponder_interval = 300
local _transponder_counter = 0
local _transponder_ident = "Unknown Vessel"
local _transponder_stance = "FRIENDLY"

if fs.exists("/etc/hostname") then
    local name = fs.cat("/etc/hostname")
    if name ~= nil and type(name) == "string" then
        name = name:match("^%s*(.-)%s*$")  -- trim
        if #name > 0 then
            _transponder_ident = name
        end
    end
end

-- (intentionally omit the Rhai script's debug `print` to keep test
-- output clean — the Rhai version uses it because Rhai print buffers
-- to the terminal screen)

while true do
    _transponder_counter = _transponder_counter + 1
    if _transponder_counter >= _transponder_interval then
        _transponder_counter = 0
        local msg = squak.presence("human", _transponder_ident, _transponder_stance)
        squak.send(950, msg)
    end
    coroutine.yield()
end
"#;

/// Spawn the transponder script as a coroutine in the given Lua state.
/// Returns the thread handle, ready to be resumed.
fn spawn_transponder(lua: &mlua::Lua) -> mlua::Result<mlua::Thread> {
    let chunk = lua.load(TRANSPONDER_LUA);
    let func: mlua::Function = chunk.into_function()?;
    lua.create_thread(func)
}

/// Run the kernel-style loop: resume the coroutine N times, each
/// resume advancing the script by one yield. After N ticks the
/// coroutine should be suspended (waiting on the next yield).
fn tick_n(thread: &mlua::Thread, n: usize) -> mlua::Result<()> {
    for _ in 0..n {
        thread.resume::<()>(())?;
    }
    Ok(())
}

// ============================================================
// THE SPIKE TEST
// ============================================================
#[test]
fn transponder_persists_across_lua_states_and_continues() {
    let log = Arc::new(Mutex::new(Vec::<Broadcast>::new()));

    // === Phase 1: original Lua state ===
    let mut eris_a = ErisLua::new().unwrap();
    setup_host_stubs(&mut eris_a, log.clone()).unwrap();

    let thread = spawn_transponder(eris_a.lua()).unwrap();
    eris_a.lua().globals().set("transponder", thread.clone()).unwrap();

    // Run for 700 ticks — that's 2 broadcast intervals (at 300 each)
    // plus 100 ticks into the third interval.
    tick_n(&thread, 700).unwrap();

    {
        let log_snapshot = log.lock().unwrap();
        assert_eq!(
            log_snapshot.len(),
            2,
            "expected 2 broadcasts after 700 ticks, got {}",
            log_snapshot.len()
        );
        assert_eq!(log_snapshot[0].freq, 950);
        assert_eq!(log_snapshot[0].ident, "HMS Spike");
        assert_eq!(log_snapshot[0].stance, "FRIENDLY");
        assert_eq!(log_snapshot[1].ident, "HMS Spike");
    }

    // Persist the suspended coroutine.
    let thread_value: mlua::Value = eris_a.lua().globals().get("transponder").unwrap();
    let blob = eris_a.persist(thread_value).unwrap();
    eprintln!("transponder service persisted: {} bytes", blob.len());

    // Drop the original state to prove no shared memory.
    drop(eris_a);

    // === Phase 2: fresh Lua state, restore, continue ticking ===
    let mut eris_b = ErisLua::new().unwrap();
    setup_host_stubs(&mut eris_b, log.clone()).unwrap();

    let restored: mlua::Value = eris_b.unpersist(&blob).unwrap();
    let thread_b: mlua::Thread = match restored {
        mlua::Value::Thread(t) => t,
        other => panic!("expected Thread from unpersist, got {:?}", other),
    };

    // Counter was at 100 of 300 before persist. Tick another 200 to
    // hit the THIRD broadcast.
    tick_n(&thread_b, 200).unwrap();

    {
        let log_snapshot = log.lock().unwrap();
        assert_eq!(
            log_snapshot.len(),
            3,
            "expected 3 broadcasts after restore + 200 more ticks, got {} (counter state was lost?)",
            log_snapshot.len()
        );
        // The third broadcast should still report HMS Spike — proving
        // the local variable `_transponder_ident` was preserved
        // through persistence.
        assert_eq!(log_snapshot[2].ident, "HMS Spike");
    }

    // Tick another 600 — should produce 2 more broadcasts (at counter
    // 300 and counter 600 from the restore point). Total: 5.
    tick_n(&thread_b, 600).unwrap();
    {
        let log_snapshot = log.lock().unwrap();
        assert_eq!(
            log_snapshot.len(),
            5,
            "expected 5 broadcasts after 600 more ticks, got {}",
            log_snapshot.len()
        );
        for (i, b) in log_snapshot.iter().enumerate() {
            assert_eq!(b.freq, 950, "broadcast {} freq", i);
            assert_eq!(b.ident, "HMS Spike", "broadcast {} ident", i);
        }
    }
}

// ============================================================
// Sanity: the script also runs to completion if you don't yield
// (proves the syntax port is correct in isolation, separate from
// persistence).
// ============================================================
#[test]
fn transponder_runs_correctly_without_persist() {
    let log = Arc::new(Mutex::new(Vec::<Broadcast>::new()));
    let mut eris = ErisLua::new().unwrap();
    setup_host_stubs(&mut eris, log.clone()).unwrap();

    let thread = spawn_transponder(eris.lua()).unwrap();
    tick_n(&thread, 1000).unwrap();

    let log_snapshot = log.lock().unwrap();
    assert_eq!(
        log_snapshot.len(),
        3,
        "expected 3 broadcasts from 1000 ticks (one per 300), got {}",
        log_snapshot.len()
    );
}
