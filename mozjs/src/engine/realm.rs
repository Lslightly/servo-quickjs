/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Realms (one QuickJS context each) and globals. A `JS::Compartment*` is the same pointer as the
//! `JS::Realm*`: the shim has one compartment per realm and no wrappers.

use std::collections::HashMap;
use std::ptr;

use quickjs_sys as q;

use super::{val_obj, Cx, RealmData};
use crate::sys::jsapi as root;

thread_local! {
    /// Global object -> its realm. Realms live until their runtime is destroyed.
    static GLOBALS: std::cell::RefCell<HashMap<usize, *mut RealmData>> = Default::default();
}

pub(crate) fn realm_of_global(global: *mut root::JSObject) -> *mut RealmData {
    GLOBALS.with(|g| g.borrow().get(&(global as usize)).copied().unwrap_or(ptr::null_mut()))
}

/// The realm of `obj`: exact for globals, otherwise the current realm (QuickJS objects do not
/// record their realm; shim classes will store it in their object data).
pub(crate) fn realm_of_object(cx: &Cx, obj: *mut root::JSObject) -> *mut RealmData {
    let realm = realm_of_global(obj);
    if realm.is_null() { cx.current_realm() } else { realm }
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
    // TODO(qjs): make the global an instance of `clasp` (patch Q1) so it gets slots and hooks.
    let _ = (clasp, hookOption, options);
    let cx = Cx::from_raw(cx);
    let qctx = q::JS_NewContext(cx.rt);
    if qctx.is_null() {
        return ptr::null_mut();
    }
    let global_val = q::JS_GetGlobalObject(qctx);
    let global = val_obj(global_val);
    // The context keeps its global alive.
    q::JS_FreeValue(qctx, global_val);
    let realm = Box::into_raw(Box::new(RealmData {
        qctx,
        global,
        private: ptr::null_mut(),
        principals,
    }));
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
    let realm = realm_of_object(cx, target);
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
    realm_of_object(super::current(), obj) as *mut root::JS::Realm
}

pub(crate) unsafe fn GetNonCCWObjectGlobal(obj: *mut root::JSObject) -> *mut root::JSObject {
    let realm = realm_of_object(super::current(), obj);
    if realm.is_null() { ptr::null_mut() } else { (*realm).global }
}

pub(crate) unsafe fn GetRealmPrivate(realm: *mut root::JS::Realm) -> *mut ::std::os::raw::c_void {
    (*(realm as *mut RealmData)).private
}

pub(crate) unsafe fn SetRealmPrivate(realm: *mut root::JS::Realm, data: *mut ::std::os::raw::c_void) {
    (*(realm as *mut RealmData)).private = data;
}

pub(crate) unsafe fn JS_GlobalObjectTraceHook(trc: *mut root::JSTracer, global: *mut root::JSObject) {
    let _ = (trc, global);
}
