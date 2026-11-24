/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Destroying a runtime frees every object left, whatever references it. Checked by ASan: no
//! object is freed twice.

use std::ptr;
use std::sync::{LazyLock, Mutex};

use mozjs::context::JSContext;
use mozjs::jsapi::{Heap, OnNewGlobalHookOption};
use mozjs::jsval::{JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_GetElement, JS_NewGlobalObject};
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject, JSEngine, JSEngineHandle, RealmOptions,
    Runtime, SIMPLE_GLOBAL_CLASS,
};

static ENGINE: LazyLock<JSEngineHandle> = LazyLock::new(|| {
    let engine = JSEngine::init().unwrap();
    let handle = engine.handle();
    std::mem::forget(engine);
    handle
});

/// Counts from Rust, never released, as Servo's are when its runtime is destroyed.
static LEFT_OVER: Mutex<Vec<usize>> = Mutex::new(Vec::new());

fn leave_counted(v: JSVal) {
    LEFT_OVER
        .lock()
        .unwrap()
        .push(Box::into_raw(Heap::boxed(v)) as usize);
}

unsafe fn eval(cx: &mut JSContext, global: HandleObject, src: &str) -> JSVal {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(cx, c"test.js".to_owned(), 1);
    evaluate_script(cx, global, src, rval.handle_mut(), options).unwrap();
    rval.get()
}

/// Destroys a runtime after `src` ran in it, with Rust counting the result.
fn teardown_after(src: &str) {
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
        let (global, cx) = realm.global_and_reborrow();
        leave_counted(eval(cx, global, src));
        // Keys of the array, counted by Rust as well.
        if src.contains("RUST_COUNTED") {
            rooted!(&in(cx) let keys = eval(cx, global, "RUST_COUNTED").to_object());
            for i in 0..50 {
                rooted!(&in(cx) let mut key = UndefinedValue());
                assert!(JS_GetElement(cx, keys.handle(), i, key.handle_mut()));
                leave_counted(key.get());
            }
            eval(cx, global, "RUST_COUNTED.length = 0");
        }
    }
    drop(runtime);
}

#[test]
fn weak_map_keys_freed_at_teardown() {
    // At teardown the array is freed first and drops the keys' counts to zero, then the
    // WeakMap releases its weak references to keys that are still queued for freeing.
    teardown_after(
        "const H = [], W = new WeakMap();
         for (let i = 0; i < 100; i++) { const K = {}; H.push(K); W.set(K, i); }
         [H, W]",
    );
}

#[test]
fn symbol_keys_freed_at_teardown() {
    teardown_after(
        "const S = [], W = new WeakMap(), WS = new WeakSet();
         for (let i = 0; i < 100; i++) { const k = Symbol(i); S.push(k); W.set(k, { i }); WS.add(k); }
         for (let i = 0; i < 100; i++) { W.set(Symbol(), {}); }
         [S, W, WS]",
    );
}

#[test]
fn weak_ref_targets_freed_at_teardown() {
    teardown_after(
        "const H = [], R = [];
         for (let i = 0; i < 100; i++) { const k = {}; H.push(k); R.push(new WeakRef(k)); }
         for (let i = 0; i < 100; i++) { R.push(new WeakRef({})); }
         for (let i = 0; i < 50; i++) { const s = Symbol(); H.push(s); R.push(new WeakRef(s)); }
         [H, R]",
    );
}

#[test]
fn finalization_registry_targets_freed_at_teardown() {
    teardown_after(
        "const H = [], F = new FinalizationRegistry(() => {});
         for (let i = 0; i < 100; i++) {
           const k = {}, t = {}; H.push(k, t); F.register(k, { held: k }, t);
         }
         for (let i = 0; i < 100; i++) { const t = {}; F.register({}, i, t); H.push(t); }
         for (let i = 0; i < 50; i++) { const k = {}; F.register(k, F, k); H.push(k); }
         [H, F]",
    );
}

#[test]
fn cycles_through_weak_maps_freed_at_teardown() {
    teardown_after(
        "const W = new WeakMap(), keep = [];
         for (let i = 0; i < 100; i++) { const k = { W }; W.set(k, { k, W }); keep.push(k); }
         const W2 = new WeakMap(); W2.set(W2, W2); W.set(W, W2);
         keep",
    );
}

#[test]
fn rust_counted_weak_keys_freed_at_teardown() {
    teardown_after(
        "globalThis.W = new WeakMap(); globalThis.R = []; globalThis.RUST_COUNTED = [];
         for (let i = 0; i < 50; i++) {
           const k = {}; W.set(k, i); R.push(new WeakRef(k)); RUST_COUNTED.push(k);
         }
         R",
    );
}
