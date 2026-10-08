/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Weak references to an object freed in a cycle while Rust still counts a reference to it, as
//! Servo's `Dom<T>` skips releasing an object that was finalized before its holder (patch Q20).

use std::cell::RefCell;
use std::ptr;
use std::sync::atomic::{AtomicPtr, Ordering};

use mozjs::glue::CallValueTracer;
use mozjs::jsapi::{GCReason, Heap, JSClass, JSClassOps, JSObject, JSTracer, OnNewGlobalHookOption};
use mozjs::jsval::{JSVal, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{JS_GC, JS_NewGlobalObject, JS_NewObject, JS_SetProperty};
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
};

thread_local! {
    /// The holder's counted reference to the target, never released.
    static FIELD: RefCell<Option<Box<Heap<JSVal>>>> = const { RefCell::new(None) };
}

/// The field once its target is freed, never dropped: releasing the count would touch freed memory.
static LEFT_OVER: AtomicPtr<Heap<JSVal>> = AtomicPtr::new(ptr::null_mut());

unsafe extern "C" fn trace(trc: *mut JSTracer, _obj: *mut JSObject) {
    FIELD.with(|field| {
        if let Some(heap) = field.borrow().as_ref() {
            CallValueTracer(trc, &**heap as *const _ as *mut _, c"target".as_ptr());
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

static HOLDER_CLASS: JSClass = JSClass {
    name: c"Holder".as_ptr(),
    flags: 0,
    cOps: &OPS,
    spec: ptr::null(),
    ext: ptr::null(),
    oOps: ptr::null(),
};

#[test]
fn freed_target_is_not_live() {
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
        let mut eval = |cx: &mut mozjs::context::JSContext, source: &str| {
            rooted!(&in(cx) let mut rval = UndefinedValue());
            let options = CompileOptionsWrapper::new(cx, c"test.js".to_owned(), 1);
            evaluate_script(cx, global, source, rval.handle_mut(), options).unwrap();
            rval.get()
        };
        {
            // target.holder = holder, and the holder's Rust field counts and traces the target.
            rooted!(&in(cx) let target = eval(cx, "(() => { const target = {}; globalThis.ref = new WeakRef(target); return target; })()").to_object());
            rooted!(&in(cx) let holder = ObjectValue(JS_NewObject(cx, &HOLDER_CLASS)));
            assert!(JS_SetProperty(cx, target.handle(), c"holder".as_ptr(), holder.handle()));
            FIELD.with(|field| *field.borrow_mut() = Some(Heap::boxed(ObjectValue(target.get()))));
        }
        mozjs::quickjs::drain_autorelease_pool();
        JS_GC(cx, GCReason::API);

        // The cycle is freed, the target keeps the count of the field.
        assert!(eval(cx, "ref.deref() === undefined").to_boolean());
        // Freeing the weak reference frees what is left of the target (checked by ASan).
        eval(cx, "delete globalThis.ref");
        mozjs::quickjs::drain_autorelease_pool();
        JS_GC(cx, GCReason::API);
        FIELD.with(|field| {
            LEFT_OVER.store(Box::into_raw(field.borrow_mut().take().unwrap()), Ordering::Relaxed)
        });
    }
}
