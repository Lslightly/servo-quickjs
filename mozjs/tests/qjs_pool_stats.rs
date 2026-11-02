/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Autorelease pool statistics, in a build with `QJS_POOL_STATS=1` (`just test-pool-stats`).

#![cfg(qjs_pool_stats)]

use std::ptr;
use std::sync::LazyLock;
use std::time::Duration;

use mozjs::jsapi::{Heap, JSObject, OnNewGlobalHookOption};
use mozjs::quickjs::{drain_autorelease_pool, pool_stats_json};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_NewGlobalObject, JS_NewPlainObject};
use mozjs::rust::{JSEngine, JSEngineHandle, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

/// The engine can only be initialized once per process; it is shared by the tests.
static ENGINE: LazyLock<JSEngineHandle> = LazyLock::new(|| {
    let engine = JSEngine::init().unwrap();
    let handle = engine.handle();
    std::mem::forget(engine);
    handle
});

#[inline(never)]
unsafe fn store_unrooted(cx: &mut mozjs::context::JSContext) -> Box<Heap<*mut JSObject>> {
    Heap::boxed(JS_NewPlainObject(cx))
}

/// Whether braces and brackets balance outside strings. Enough to catch a malformed array such
/// as `"hist":[}` without pulling in a JSON parser.
fn balanced(json: &str) -> bool {
    let mut stack = Vec::new();
    let (mut in_string, mut escape) = (false, false);
    for c in json.chars() {
        if in_string {
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                if stack.pop() != Some(c) {
                    return false;
                }
            }
            _ => {}
        }
    }
    stack.is_empty() && !in_string
}

fn elapsed_ms(json: &str) -> u64 {
    json.split("\"elapsed_ms\":")
        .nth(1)
        .and_then(|rest| rest.split([',', '}']).next())
        .and_then(|n| n.parse().ok())
        .expect("elapsed_ms field")
}

#[test]
fn fates_of_raw_results() {
    std::env::set_var("QJS_POOL_STATS_SAMPLE", "1");
    std::env::set_var("QJS_POOL_STATS_OUT", "/dev/null");
    let mut runtime = Runtime::new(ENGINE.clone());
    let cx = runtime.cx();
    unsafe {
        rooted!(&in(cx) let global = JS_NewGlobalObject(
            cx,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(cx, global.handle());
        let cx = &mut *realm;
        drain_autorelease_pool();
        let before = pool_stats_json();
        assert!(!before.contains("\"api\":\"JS_NewPlainObject\""));

        rooted!(&in(cx) let rooted = JS_NewPlainObject(cx));
        let kept = store_unrooted(cx);
        assert!(!JS_NewPlainObject(cx).is_null());
        drain_autorelease_pool();

        let json = pool_stats_json();
        assert!(json.contains("\"elapsed_ms\":"), "{json}");
        assert!(
            json.contains(
                "\"api\":\"JS_NewPlainObject\",\"raw\":true,\"site\":null,\"kind\":\"result\",\
                 \"total\":3,\"fresh\":3,\"rooted\":1,\"kept\":1,\"dropped\":1"
            ),
            "{json}"
        );
        // The unrooted ones name their caller; the rooted one isn't sampled.
        assert!(
            json.contains("\"fate\":\"kept\",\"frames\":[\"qjs_pool_stats::store_unrooted"),
            "{json}"
        );
        assert!(
            json.contains(
                "\"fate\":\"dropped\",\"frames\":[\"qjs_pool_stats::fates_of_raw_results"
            ),
            "{json}"
        );
        assert!(!json.contains("\"fate\":\"rooted\""), "{json}");
        // Drains are timed per context, and the result stays well-formed JSON: an unclosed
        // histogram array would make every line unreadable to the analysis tools.
        assert!(balanced(&json), "{json}");
        assert!(json.contains("\"ctx\":\"scope_drop\""), "{json}");
        let task = json.split("\"ctx\":\"task\"").nth(1).unwrap_or_default();
        assert!(task.contains("\"calls\":2,\"empty\":0,"), "{json}");
        assert!(task.contains("\"hist\":[{\"lo_ns\":"), "{json}");
        drop(kept);
    }
}

/// `elapsed_ms` counts from the first push of each runtime, not from process start.
#[test]
fn elapsed_ms_resets_per_runtime() {
    std::env::set_var("QJS_POOL_STATS_OUT", "/dev/null");
    std::env::set_var("QJS_POOL_STATS_SAMPLE", "0");

    let first = {
        let mut runtime = Runtime::new(ENGINE.clone());
        let cx = runtime.cx();
        unsafe {
            rooted!(&in(cx) let global = JS_NewGlobalObject(
                cx,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = AutoRealm::new_from_handle(cx, global.handle());
            let cx = &mut *realm;
            assert!(!JS_NewPlainObject(cx).is_null());
        }
        std::thread::sleep(Duration::from_millis(40));
        elapsed_ms(&pool_stats_json())
    };
    assert!(first >= 40, "{first}");

    // Dropping the runtime writes its final line and resets the statistics.
    let mut runtime = Runtime::new(ENGINE.clone());
    let cx = runtime.cx();
    unsafe {
        rooted!(&in(cx) let global = JS_NewGlobalObject(
            cx,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(cx, global.handle());
        let cx = &mut *realm;
        assert!(!JS_NewPlainObject(cx).is_null());
    }
    let second = elapsed_ms(&pool_stats_json());
    assert!(second < 40, "{second}");
    assert!(second < first, "{second} < {first}");
}
