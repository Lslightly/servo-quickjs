/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Realms (one QuickJS context each) and globals. A `JS::Compartment*` is the same pointer as the
//! `JS::Realm*`: the shim has one compartment per realm and no wrappers.
//!
//! The global object is an instance of its SpiderMonkey class (patch Q1), so its reserved slots,
//! trace hook and finalizer work as for other objects of that class; resolve and enumerate hooks
//! run from exotic methods (`proxy.rs`). The global `this` is the WindowProxy (Q2,
//! `SetWindowProxy`).

use std::collections::HashMap;
use std::ptr;

use quickjs_sys as q;

use super::{obj_val, val_obj, Cx, RealmData};
use crate::sys::jsapi as root;

thread_local! {
    /// Global object -> its realm. Realms live until their runtime is destroyed.
    static GLOBALS: std::cell::RefCell<HashMap<usize, *mut RealmData>> = Default::default();
}

pub(crate) fn realm_of_global(global: *mut root::JSObject) -> *mut RealmData {
    GLOBALS.with(|g| g.borrow().get(&(global as usize)).copied().unwrap_or(ptr::null_mut()))
}

/// The realm of `obj`: exact for globals, otherwise derived from the function or prototype chain
/// (QuickJS objects do not record their realm), falling back to the current realm.
pub(crate) fn realm_of_object(cx: &Cx, obj: *mut root::JSObject) -> *mut RealmData {
    let realm = realm_of_global(obj);
    if !realm.is_null() {
        return realm;
    }
    let realm = unsafe { realm_of_plain_object(cx, obj) };
    if realm.is_null() { cx.current_realm() } else { realm }
}

pub(crate) fn realm_of_qctx(qctx: *mut q::JSContext) -> *mut RealmData {
    all_realms()
        .into_iter()
        .find(|realm| unsafe { (**realm).qctx } == qctx)
        .unwrap_or(ptr::null_mut())
}

/// QuickJS: functions know their realm; other objects get the realm of the first prototype whose
/// realm is known, or whose chain ends at a realm's `Object.prototype`.
unsafe fn realm_of_plain_object(cx: &Cx, obj: *mut root::JSObject) -> *mut RealmData {
    if q::JS_IsFunction(cx.base_ctx, obj_val(obj)) != 0 {
        let realm = realm_of_qctx(q::JS_GetFunctionRealm(cx.base_ctx, obj_val(obj)));
        if !realm.is_null() {
            return realm;
        }
    }
    let mut current = obj;
    loop {
        let proto = q::JS_GetStaticPrototype(cx.base_ctx, obj_val(current));
        if proto.tag() != q::JS_TAG_OBJECT {
            break;
        }
        let proto = val_obj(proto);
        q::JS_FreeValueRT(cx.rt, obj_val(proto));
        let realm = super::class::known_object_realm(proto);
        if !realm.is_null() {
            return realm;
        }
        current = proto;
    }
    if current == obj {
        return ptr::null_mut();
    }
    all_realms()
        .into_iter()
        .find(|realm| {
            let object_proto = q::JS_GetClassProto((**realm).qctx, 1);
            q::JS_FreeValueRT(cx.rt, object_proto);
            object_proto.tag() == q::JS_TAG_OBJECT && val_obj(object_proto) == current
        })
        .unwrap_or(ptr::null_mut())
}

pub(crate) fn all_realms() -> Vec<*mut RealmData> {
    GLOBALS.with(|g| g.borrow().values().copied().collect())
}

pub(crate) fn take_all_realms(_cx: &mut Cx) -> Vec<Box<RealmData>> {
    GLOBALS.with(|g| {
        g.borrow_mut()
            .drain()
            .map(|(_, realm)| unsafe { Box::from_raw(realm) })
            .collect()
    })
}

pub(crate) unsafe fn JS_NewRealmOptions() -> *mut root::JS::RealmOptions {
    Box::into_raw(Box::new(std::mem::zeroed()))
}

pub(crate) unsafe fn DeleteRealmOptions(options: *mut root::JS::RealmOptions) {
    drop(Box::from_raw(options));
}

pub(crate) unsafe fn JS_NewGlobalObject(cx: *mut root::JSContext, clasp: *const root::JSClass, principals: *mut root::JSPrincipals, hookOption: root::JS::OnNewGlobalHookOption, options: *const root::JS::RealmOptions) -> *mut root::JSObject {
    let _ = hookOption;
    let cx = Cx::from_raw(cx);
    let qctx = q::JS_NewContext(cx.rt);
    if qctx.is_null() {
        return ptr::null_mut();
    }
    let class_id = super::class::class_id(cx.rt, clasp);
    assert_eq!(q::JS_SetGlobalObjectClass(qctx, class_id), 0, "global classes cannot be callable");
    let global_val = q::JS_GetGlobalObject(qctx);
    let global = val_obj(global_val);
    // The context keeps its global alive.
    q::JS_FreeValue(qctx, global_val);
    let (creation_options, behaviors) = if options.is_null() {
        (std::mem::zeroed(), std::mem::zeroed())
    } else {
        (ptr::read(&(*options).creationOptions_), ptr::read(&(*options).behaviors_))
    };
    if !principals.is_null() {
        super::JS_HoldPrincipals(principals);
    }
    let realm = Box::into_raw(Box::new(RealmData {
        qctx,
        global,
        private: ptr::null_mut(),
        principals,
        creation_options,
        behaviors,
        window_proxy: ptr::null_mut(),
    }));
    let data = super::class::ObjectData::new(clasp, realm);
    q::JS_SetOpaque(obj_val(global), Box::into_raw(data) as *mut _);
    q::JS_SetContextOpaque(qctx, realm as *mut _);
    GLOBALS.with(|g| g.borrow_mut().insert(global as usize, realm));
    global
}

pub(crate) unsafe fn JS_FireOnNewGlobalObject(cx: *mut root::JSContext, global: root::JS::HandleObject) {
    let _ = (cx, global);
}

pub(crate) unsafe fn EnterRealm(cx: *mut root::JSContext, target: *mut root::JSObject) -> *mut root::JS::Realm {
    let cx = Cx::from_raw(cx);
    let old = cx.current_realm();
    let realm = super::class::object_realm(cx, target);
    assert!(!realm.is_null(), "EnterRealm on an object without a known realm");
    cx.realm_stack.push(realm);
    old as *mut root::JS::Realm
}

pub(crate) unsafe fn LeaveRealm(cx: *mut root::JSContext, oldRealm: *mut root::JS::Realm) {
    let cx = Cx::from_raw(cx);
    cx.realm_stack.pop().expect("LeaveRealm without EnterRealm");
    debug_assert_eq!(cx.current_realm() as *mut root::JS::Realm, oldRealm);
}

pub(crate) unsafe fn GetCurrentRealmOrNull(cx: *mut root::JSContext) -> *mut root::JS::Realm {
    Cx::from_raw(cx).current_realm() as *mut root::JS::Realm
}

pub(crate) unsafe fn CurrentGlobalOrNull(cx: *mut root::JSContext) -> *mut root::JSObject {
    let realm = Cx::from_raw(cx).current_realm();
    if realm.is_null() { ptr::null_mut() } else { (*realm).global }
}

pub(crate) unsafe fn GetRealmGlobalOrNull(realm: *mut root::JS::Realm) -> *mut root::JSObject {
    if realm.is_null() { ptr::null_mut() } else { (*(realm as *mut RealmData)).global }
}

pub(crate) unsafe fn GetObjectRealmOrNull(obj: *mut root::JSObject) -> *mut root::JS::Realm {
    super::class::object_realm(super::current(), obj) as *mut root::JS::Realm
}

pub(crate) unsafe fn GetNonCCWObjectGlobal(obj: *mut root::JSObject) -> *mut root::JSObject {
    let realm = super::class::object_realm(super::current(), obj);
    if realm.is_null() { ptr::null_mut() } else { (*realm).global }
}

pub(crate) unsafe fn GetRealmPrivate(realm: *mut root::JS::Realm) -> *mut ::std::os::raw::c_void {
    (*(realm as *mut RealmData)).private
}

pub(crate) unsafe fn SetRealmPrivate(realm: *mut root::JS::Realm, data: *mut ::std::os::raw::c_void) {
    (*(realm as *mut RealmData)).private = data;
}

pub(crate) unsafe fn JS_GlobalObjectTraceHook(trc: *mut root::JSTracer, global: *mut root::JSObject) {
    let realm = realm_of_global(global);
    if realm.is_null() {
        return;
    }
    if let Some(trace) = (*realm).creation_options.traceGlobal_ {
        trace(trc, global);
    }
}
