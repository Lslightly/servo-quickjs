/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Engine and runtime lifetime, GC and stack configuration.

use std::ptr;

use quickjs_sys as q;

use super::{set_current, Cx};
use crate::sys::jsapi as root;

pub(crate) unsafe fn JS_Init() -> bool {
    true
}

pub(crate) unsafe fn JS_ShutDown() {}

pub(crate) unsafe fn JS_NewContext(maxbytes: u32, parentRuntime: *mut root::JSRuntime) -> *mut root::JSContext {
    let _ = (maxbytes, parentRuntime);
    let rt = q::JS_NewRuntime();
    if rt.is_null() {
        return ptr::null_mut();
    }
    let base_ctx = q::JS_NewContext(rt);
    let cx = Box::into_raw(Box::new(Cx {
        rt,
        base_ctx,
        realm_stack: Vec::new(),
        pool: Vec::new(),
        atom_pool: Vec::new(),
        warning_reporter: None,
    }));
    q::JS_SetRuntimeOpaque(rt, cx as *mut _);
    set_current(cx);
    cx as *mut root::JSContext
}

pub(crate) unsafe fn JS_DestroyContext(cx: *mut root::JSContext) {
    let cx = Box::from_raw(cx as *mut Cx);
    let mut cx = cx;
    cx.drain_pool((0, 0));
    for mut realm in super::realm::take_all_realms(&mut cx) {
        if let Some(data) = realm.global_data.take() {
            data.free(cx.rt);
        }
        q::JS_FreeContext(realm.qctx);
    }
    q::JS_FreeContext(cx.base_ctx);
    q::JS_FreeRuntime(cx.rt);
    set_current(ptr::null_mut());
}

pub(crate) unsafe fn JS_GetRuntime(cx: *mut root::JSContext) -> *mut root::JSRuntime {
    Cx::from_raw(cx).rt as *mut root::JSRuntime
}

pub(crate) unsafe fn JS_SetGCParameter(cx: *mut root::JSContext, key: root::JSGCParamKey, value: u32) {
    let _ = (cx, key, value);
}

pub(crate) unsafe fn JS_AddExtraGCRootsTracer(cx: *mut root::JSContext, traceOp: root::JSTraceDataOp, data: *mut ::std::os::raw::c_void) -> bool {
    // Rust-side roots hold reference counts instead of being traced (see the design doc).
    let _ = (cx, traceOp, data);
    true
}

pub(crate) unsafe fn JS_SetNativeStackQuota(cx: *mut root::JSContext, systemCodeStackSize: root::JS::NativeStackSize, trustedScriptStackSize: root::JS::NativeStackSize, untrustedScriptStackSize: root::JS::NativeStackSize) {
    let _ = (systemCodeStackSize, trustedScriptStackSize);
    let cx = Cx::from_raw(cx);
    q::JS_SetMaxStackSize(cx.rt, untrustedScriptStackSize as usize);
    q::JS_UpdateStackTop(cx.rt);
}

pub(crate) unsafe fn InitSelfHostedCode(cx: *mut root::JSContext, cache: root::JS::SelfHostedCache, writer: root::JS::SelfHostedWriter) -> bool {
    let _ = (cx, cache, writer);
    true
}

pub(crate) unsafe fn SetWarningReporter(cx: *mut root::JSContext, reporter: root::JS::WarningReporter) -> root::JS::WarningReporter {
    std::mem::replace(&mut Cx::from_raw(cx).warning_reporter, reporter)
}

pub(crate) unsafe fn JS_GC(cx: *mut root::JSContext, reason: root::JS::GCReason) {
    let _ = reason;
    q::JS_RunGC(Cx::from_raw(cx).rt);
}
