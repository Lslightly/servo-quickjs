/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The tracing verifier (`QJS_VERIFY_TRACING=1`) aborts when a trace hook marks an object more
//! often than Rust holds counted references to it.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::ptr;

use mozjs::glue::CallValueTracer;
use mozjs::jsapi::{
    GCReason, Heap, JSClass, JSClassOps, JSObject, JSTracer, OnNewGlobalHookOption,
};
use mozjs::jsval::{JSVal, ObjectValue};
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_NewGlobalObject, JS_NewObject, JS_NewPlainObject, JS_GC};
use mozjs::rust::{JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};

thread_local! {
    static FIELD: RefCell<Option<Box<Heap<JSVal>>>> = const { RefCell::new(None) };
    static MARKS: Cell<usize> = const { Cell::new(1) };
}

unsafe extern "C" fn trace(trc: *mut JSTracer, _obj: *mut JSObject) {
    FIELD.with(|field| {
        if let Some(heap) = field.borrow().as_ref() {
            for _ in 0..MARKS.with(Cell::get) {
                CallValueTracer(trc, &**heap as *const _ as *mut _, c"field".as_ptr());
            }
        }
    });
}

static OPS: JSClassOps = JSClassOps {
    addProperty: None,
    delProperty: None,
    enumerate: None,
    newEnumerate: None,
    resolve: None,
    mayResolve: None,
    finalize: None,
    call: None,
    construct: None,
    trace: Some(trace),
};

static TRACED_CLASS: JSClass = JSClass {
    name: c"Traced".as_ptr(),
    flags: 0,
    cOps: &OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

/// An object of `TRACED_CLASS` whose trace hook marks one `Heap` field `marks` times.
fn collect_with_marks(marks: usize) {
    MARKS.with(|m| m.set(marks));
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
        let mut realm = mozjs::realm::AutoRealm::new_from_handle(cx, global.handle());
        let cx = &mut *realm;
        rooted!(&in(cx) let holder = JS_NewObject(cx, &TRACED_CLASS));
        rooted!(&in(cx) let target = JS_NewPlainObject(cx));
        FIELD.with(|field| *field.borrow_mut() = Some(Heap::boxed(ObjectValue(target.get()))));
        let _ = holder;
        JS_GC(cx, GCReason::API);
        FIELD.with(|field| field.borrow_mut().take());
    }
}

#[test]
fn exact_tracing_passes() {
    // SAFETY: the test sets the variable before any runtime exists on this thread.
    unsafe { std::env::set_var("QJS_VERIFY_TRACING", "1") };
    collect_with_marks(1);
}

#[test]
#[ignore = "run by over_marking_aborts in a child process"]
fn over_marking_child() {
    collect_with_marks(2);
}

#[test]
fn over_marking_aborts() {
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "over_marking_child", "--ignored", "--nocapture"])
        .env("QJS_VERIFY_TRACING", "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(
        stderr.contains("marked 2 times from Rust trace hooks in one GC pass, but Rust holds 1"),
        "{stderr}"
    );
}
