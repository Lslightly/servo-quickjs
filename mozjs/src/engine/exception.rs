/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! The pending exception. QuickJS keeps it per runtime; SpiderMonkey's "get" does not clear it, so
//! reads take it and throw it again.

use servo_quickjs as q;

use super::Cx;
use crate::sys::jsapi as root;
use crate::sys::jsval::JSVal;

/// Clears an uncatchable exception (script termination, e.g. by an interrupt callback): for
/// SpiderMonkey, termination is a failure without a pending exception.
unsafe fn clear_uncatchable(cx: &Cx) {
    if q::JS_HasUncatchableException(cx.qctx()) != 0 {
        q::JS_FreeValueRT(cx.rt, q::JS_GetException(cx.qctx()));
    }
}

pub(crate) unsafe fn JS_IsExceptionPending(cx: *mut root::JSContext) -> bool {
    let cx = Cx::from_raw(cx);
    clear_uncatchable(cx);
    q::JS_HasException(cx.qctx()) != 0
}

pub(crate) unsafe fn JS_GetPendingException(cx: *mut root::JSContext, vp: root::JS::MutableHandleValue) -> bool {
    let cx = Cx::from_raw(cx);
    clear_uncatchable(cx);
    let qctx = cx.qctx();
    if q::JS_HasException(qctx) == 0 {
        return false;
    }
    let exception = q::JS_GetException(qctx);
    q::JS_Throw(qctx, q::JS_DupValueRT(cx.rt, exception));
    *vp.ptr = cx.adopt(exception);
    true
}

pub(crate) unsafe fn JS_SetPendingException(cx: *mut root::JSContext, v: root::JS::HandleValue, behavior: root::JS::ExceptionStackBehavior) {
    let _ = behavior;
    let cx = Cx::from_raw(cx);
    q::JS_Throw(cx.qctx(), q::JS_DupValueRT(cx.rt, JSVal::to_qjs(*v.ptr)));
}

pub(crate) unsafe fn JS_ClearPendingException(cx: *mut root::JSContext) {
    let cx = Cx::from_raw(cx);
    let exception = q::JS_GetException(cx.qctx());
    q::JS_FreeValueRT(cx.rt, exception);
}

/// Throws a new error of the given type with `message`.
pub(crate) unsafe fn throw_error(cx: *mut root::JSContext, kind: root::JSExnType, message: &std::ffi::CStr) {
    let qctx = Cx::from_raw(cx).qctx();
    let fmt = c"%s".as_ptr();
    match kind {
        root::JSExnType::JSEXN_TYPEERR => q::JS_ThrowTypeError(qctx, fmt, message.as_ptr()),
        root::JSExnType::JSEXN_RANGEERR => q::JS_ThrowRangeError(qctx, fmt, message.as_ptr()),
        root::JSExnType::JSEXN_SYNTAXERR => q::JS_ThrowSyntaxError(qctx, fmt, message.as_ptr()),
        root::JSExnType::JSEXN_REFERENCEERR => q::JS_ThrowReferenceError(qctx, fmt, message.as_ptr()),
        _ => q::JS_ThrowInternalError(qctx, fmt, message.as_ptr()),
    };
}
