/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The shim's own operations use the builtins a realm was created with, not what its scripts
//! put in their place, and Servo's cross-origin property holder cache keeps holders in the
//! accessing realm.

use std::ptr;
use std::sync::LazyLock;

use mozjs::context::JSContext;
use mozjs::jsapi::{
    ClippedTime, Heap, JSObject, OnNewGlobalHookOption, RegExpFlags, JSPROP_ENUMERATE,
};
use mozjs::jsval::{Int32Value, JSVal, ObjectValue, UndefinedValue};
use mozjs::quickjs::{drain_autorelease_pool, realm_count};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    DateGetMsecSinceEpoch, ExecuteRegExpNoStatics, GetRealmKeyObject, GetWeakMapEntry,
    JS_DefineProperty, JS_NewGlobalObject, JS_ParseJSONWithReviver, JS_ValueToObject, MapGet,
    MapHas, MapSet, MapSize, NewDateObject, NewMapObject, NewSetObject, NewUCRegExpObject,
    NewWeakMapObject, SetAdd, SetHas, SetSize, SetWeakMapEntry, JS_GC,
};
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

unsafe fn new_global(cx: &mut JSContext) -> *mut JSObject {
    JS_NewGlobalObject(
        cx,
        &SIMPLE_GLOBAL_CLASS,
        ptr::null_mut(),
        OnNewGlobalHookOption::FireOnNewGlobalHook,
        &*RealmOptions::default(),
    )
}

unsafe fn eval(cx: &mut JSContext, global: HandleObject, src: &str) -> JSVal {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(cx, c"qjs_intrinsics".to_owned(), 1);
    assert!(
        evaluate_script(cx, global, src, rval.handle_mut(), options).is_ok(),
        "{src}"
    );
    rval.get()
}

/// Replaces the builtins the shim used to look up by name with functions that record the call.
const REPLACE_BUILTINS: &str = r#"
    globalThis.calls = [];
    const spy = name => function () { calls.push(name); throw new Error(name); };
    for (const [ctor, names] of [
        [Map, ["get", "set", "has", "delete", "clear", "keys", "values", "entries", "forEach"]],
        [Set, ["add", "has", "delete", "clear", "keys", "values", "entries", "forEach"]],
        [WeakMap, ["get", "set", "has", "delete"]],
        [Date, ["getTime"]],
        [RegExp, ["exec"]],
        [Array, ["values", "keys", "entries", "forEach"]],
    ]) {
        for (const name of names) ctor.prototype[name] = spy(`${ctor.name}.${name}`);
        Object.defineProperty(ctor.prototype, "size", { get: spy(`${ctor.name}.size`) });
        ctor.prototype.constructor = spy(ctor.name);
    }
    JSON.parse = spy("JSON.parse");
    Object.prototype.constructor = spy("Object.prototype.constructor");
    globalThis.Object = spy("Object");
"#;

#[test]
fn replaced_builtins_are_not_called() {
    let mut runtime = Runtime::new(ENGINE.clone());
    let cx = runtime.cx();
    unsafe {
        rooted!(&in(cx) let global = new_global(cx));
        let mut realm = AutoRealm::new_from_handle(cx, global.handle());
        let (global, cx) = realm.global_and_reborrow();
        eval(cx, global, REPLACE_BUILTINS);

        rooted!(&in(cx) let map = NewMapObject(cx));
        rooted!(&in(cx) let key = Int32Value(1));
        rooted!(&in(cx) let value = Int32Value(42));
        rooted!(&in(cx) let mut out = UndefinedValue());
        assert!(MapSet(cx, map.handle(), key.handle(), value.handle()));
        assert!(MapGet(cx, map.handle(), key.handle(), out.handle_mut()));
        assert_eq!(out.get().to_int32(), 42);
        let mut has = false;
        assert!(MapHas(cx, map.handle(), key.handle(), &mut has));
        assert!(has);
        assert_eq!(MapSize(cx, map.handle()), 1);

        rooted!(&in(cx) let set = NewSetObject(cx));
        assert!(SetAdd(cx, set.handle(), key.handle()));
        assert!(SetHas(cx, set.handle(), key.handle(), &mut has));
        assert!(has);
        assert_eq!(SetSize(cx, set.handle()), 1);

        rooted!(&in(cx) let date = NewDateObject(cx, ClippedTime { t: 1234.0 }));
        let mut ms = 0.0;
        assert!(DateGetMsecSinceEpoch(cx, date.handle(), &mut ms));
        assert_eq!(ms, 1234.0);

        let pattern: Vec<u16> = "a+".encode_utf16().collect();
        rooted!(&in(cx) let re = NewUCRegExpObject(cx, pattern.as_ptr(), pattern.len(), RegExpFlags { flags_: 0 }));
        assert!(!re.get().is_null());
        let input: Vec<u16> = "baa".encode_utf16().collect();
        let mut index = 0;
        assert!(ExecuteRegExpNoStatics(
            cx,
            re.handle(),
            input.as_ptr(),
            input.len(),
            &mut index,
            true,
            out.handle_mut()
        ));
        assert_eq!(index, 3);

        rooted!(&in(cx) let number = Int32Value(5));
        rooted!(&in(cx) let mut boxed = ptr::null_mut::<JSObject>());
        assert!(JS_ValueToObject(cx, number.handle(), boxed.handle_mut()));
        assert!(!boxed.get().is_null());

        rooted!(&in(cx) let reviver = eval(cx, global, "(k, v) => typeof v === 'number' ? v + 1 : v"));
        let json: Vec<u16> = "[1]".encode_utf16().collect();
        assert!(JS_ParseJSONWithReviver(
            cx,
            json.as_ptr(),
            json.len() as u32,
            reviver.handle(),
            out.handle_mut()
        ));
        assert!(out.get().is_object());

        assert!(eval(cx, global, "calls.length === 0").to_boolean());
    }
}

#[test]
fn cross_origin_holders_live_in_the_accessing_realm() {
    let mut runtime = Runtime::new(ENGINE.clone());
    let cx = runtime.cx();
    unsafe {
        let baseline = realm_count();
        let target = Heap::boxed(new_global(cx));
        let map = Heap::boxed(ptr::null_mut::<JSObject>());
        {
            rooted!(&in(cx) let target = target.get());
            rooted!(&in(cx) let accessing = new_global(cx));
            rooted!(&in(cx) let other = new_global(cx));
            for global in [target.handle(), accessing.handle()] {
                let mut realm = AutoRealm::new_from_handle(cx, global);
                let (global, cx) = realm.global_and_reborrow();
                eval(cx, global, REPLACE_BUILTINS);
            }

            // As in ensure_cross_origin_property_holder: the map in the target's realm, the key
            // and the holder (with methods) of the accessing realm.
            {
                let mut realm = AutoRealm::new_from_handle(cx, target.handle());
                map.set(NewWeakMapObject(&mut realm));
            }
            rooted!(&in(cx) let map_obj = map.get());
            assert!(mozjs::jsapi::IsWeakMapObject(map_obj.get()));
            let (key, other_key, holder) = {
                let mut realm = AutoRealm::new_from_handle(cx, accessing.handle());
                let (global, cx) = realm.global_and_reborrow();
                let key = GetRealmKeyObject(cx);
                assert_eq!(key, GetRealmKeyObject(cx));
                let holder = eval(cx, global, "({ postMessage() { return globalThis; } })");
                let mut other_realm = AutoRealm::new_from_handle(cx, other.handle());
                (key, GetRealmKeyObject(&mut other_realm), holder)
            };
            assert_ne!(key, other_key);
            rooted!(&in(cx) let key = ObjectValue(key));
            rooted!(&in(cx) let other_key = ObjectValue(other_key));
            rooted!(&in(cx) let holder = holder);
            let mut realm = AutoRealm::new_from_handle(cx, target.handle());
            let (target_global, cx) = realm.global_and_reborrow();
            rooted!(&in(cx) let mut out = UndefinedValue());
            assert!(GetWeakMapEntry(
                cx,
                map_obj.handle(),
                key.handle(),
                out.handle_mut()
            ));
            assert!(out.get().is_undefined());
            assert!(SetWeakMapEntry(
                cx,
                map_obj.handle(),
                key.handle(),
                holder.handle()
            ));
            assert!(GetWeakMapEntry(
                cx,
                map_obj.handle(),
                key.handle(),
                out.handle_mut()
            ));
            assert_eq!(out.get().to_object(), holder.get().to_object());
            assert!(GetWeakMapEntry(
                cx,
                map_obj.handle(),
                other_key.handle(),
                out.handle_mut()
            ));
            assert!(out.get().is_undefined());

            rooted!(&in(cx) let map_value = ObjectValue(map_obj.get()));
            assert!(JS_DefineProperty(
                cx,
                target_global,
                c"map".as_ptr(),
                map_value.handle(),
                JSPROP_ENUMERATE as u32
            ));
            assert!(eval(cx, target_global, "calls.length === 0").to_boolean());
            let mut accessing_realm = AutoRealm::new_from_handle(cx, accessing.handle());
            let (accessing_global, cx) = accessing_realm.global_and_reborrow();
            assert!(eval(cx, accessing_global, "calls.length === 0").to_boolean());
        }
        assert_eq!(realm_count(), baseline + 3);

        // The map lives on, and keeps neither the accessing realm nor its holder alive.
        drain_autorelease_pool();
        JS_GC(cx, mozjs::jsapi::GCReason::API);
        assert_eq!(realm_count(), baseline + 1);

        // A new realm doesn't find the freed realm's holder.
        rooted!(&in(cx) let map_obj = map.get());
        rooted!(&in(cx) let fresh = new_global(cx));
        let mut realm = AutoRealm::new_from_handle(cx, fresh.handle());
        rooted!(&in(&mut realm) let key = ObjectValue(GetRealmKeyObject(&mut realm)));
        rooted!(&in(&mut realm) let mut out = UndefinedValue());
        assert!(GetWeakMapEntry(
            &mut realm,
            map_obj.handle(),
            key.handle(),
            out.handle_mut()
        ));
        assert!(out.get().is_undefined());
    }
}
