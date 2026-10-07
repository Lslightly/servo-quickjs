/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! QuickJS shim: event-handler style compilation through an environment chain.

use std::ptr;

use mozjs::jsapi::{HandleValueArray, JS_GetFunctionObject, JS_NewPlainObject, OnNewGlobalHookOption, SupportUnscopables};
use mozjs::jsval::{Int32Value, UndefinedValue};
use mozjs::rooted;
use mozjs::rust::wrappers2::{Call, CompileFunction, JS_NewGlobalObject, JS_SetProperty};
use mozjs::rust::{
    transform_u16_to_source_text, CompileOptionsWrapper, EnvironmentChain,
    JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS,
};

#[test]
fn compile_function_with_environment_chain() {
    let engine = JSEngine::init().unwrap();
    let mut runtime = Runtime::new(engine.handle());
    let cx = runtime.cx();
    let options = RealmOptions::default();
    unsafe {
        rooted!(&in(cx) let global = JS_NewGlobalObject(
            cx,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*options,
        ));
        let mut realm = mozjs::realm::AutoRealm::new_from_handle(cx, global.handle());
        let cx = &mut *realm;

        // outer: { a: 1, b: 1 }, inner: { b: 2 } -> a + b == 3; `event` is the argument.
        rooted!(&in(cx) let outer = JS_NewPlainObject(cx.raw_cx()));
        rooted!(&in(cx) let inner = JS_NewPlainObject(cx.raw_cx()));
        rooted!(&in(cx) let one = Int32Value(1));
        rooted!(&in(cx) let two = Int32Value(2));
        assert!(JS_SetProperty(cx, outer.handle(), c"a".as_ptr(), one.handle()));
        assert!(JS_SetProperty(cx, outer.handle(), c"b".as_ptr(), one.handle()));
        assert!(JS_SetProperty(cx, inner.handle(), c"b".as_ptr(), two.handle()));

        let chain = EnvironmentChain::new(cx, SupportUnscopables::Yes);
        chain.append(outer.get());
        chain.append(inner.get());

        let body: Vec<u16> = "return a + b + event;".encode_utf16().collect();
        let args = [c"event".as_ptr()];
        let options = CompileOptionsWrapper::new(cx, c"handler.html".to_owned(), 7);
        rooted!(&in(cx) let fun = CompileFunction(
            cx,
            chain.get(),
            options.ptr,
            c"onclick".as_ptr(),
            1,
            args.as_ptr(),
            &mut transform_u16_to_source_text(&body),
        ));
        assert!(!fun.get().is_null());

        rooted!(&in(cx) let funval = mozjs::jsval::ObjectValue(JS_GetFunctionObject(fun.get())));
        rooted!(&in(cx) let this = UndefinedValue());
        rooted!(&in(cx) let mut rval = UndefinedValue());
        let argv = [Int32Value(10)];
        let argv = HandleValueArray { length_: 1, elements_: argv.as_ptr() };
        assert!(Call(cx, this.handle(), funval.handle(), &argv, rval.handle_mut()));
        assert_eq!(rval.get().to_int32(), 13);
    }
}
