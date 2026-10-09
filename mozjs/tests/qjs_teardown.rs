/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Destroying a runtime frees every object left, whatever references it.

use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use mozjs::jsapi::{Heap, OnNewGlobalHookOption};
use mozjs::jsval::{JSVal, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
};

/// A count from Rust, never released, as Servo's are when its runtime is destroyed.
static LEFT_OVER: AtomicPtr<Heap<JSVal>> = AtomicPtr::new(ptr::null_mut());

#[test]
fn weak_map_keys_freed_at_teardown() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
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
        rooted!(&in(cx) let mut rval = UndefinedValue());
        let options = CompileOptionsWrapper::new(cx, c"test.js".to_owned(), 1);
        // At teardown the array is freed first and drops the keys' counts to zero, then the
        // WeakMap releases its weak references to keys that are still queued for freeing.
        evaluate_script(
            cx,
            global,
            "const H = [], W = new WeakMap();
             for (let i = 0; i < 100; i++) { const K = {}; H.push(K); W.set(K, i); }
             [H, W]",
            rval.handle_mut(),
            options,
        )
        .unwrap();
        LEFT_OVER.store(Box::into_raw(Heap::boxed(rval.get())), Ordering::Relaxed);
    }
    drop(runtime);
    // Checked by ASan: no key is freed twice.
}
