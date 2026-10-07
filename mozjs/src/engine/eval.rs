/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Compile options and classic script evaluation.

use std::ffi::{c_char, c_uint, CString};
use std::ptr;

use quickjs_sys as q;

use super::{cstring_lossy, Cx};
use crate::sys::jsapi as root;
use crate::sys::jsval::JSVal;

/// Backs a `JS::ReadOnlyCompileOptions*`: mozjs writes the SpiderMonkey fields directly, so the
/// real struct comes first.
#[repr(C)]
struct CompileOptions {
    base: root::JS::ReadOnlyCompileOptions,
    filename: CString,
}

unsafe fn options<'a>(options: *const root::JS::ReadOnlyCompileOptions) -> &'a CompileOptions {
    &*(options as *const CompileOptions)
}

pub(crate) unsafe fn NewCompileOptions(aCx: *mut root::JSContext, aFile: *const c_char, aLine: c_uint) -> *mut root::JS::ReadOnlyCompileOptions {
    let _ = aCx;
    let mut base: root::JS::ReadOnlyCompileOptions = std::mem::zeroed();
    base.lineno = aLine;
    let options = Box::new(CompileOptions {
        base,
        filename: cstring_lossy(aFile),
    });
    Box::into_raw(options) as *mut root::JS::ReadOnlyCompileOptions
}

pub(crate) unsafe fn DeleteCompileOptions(aOpts: *mut root::JS::ReadOnlyCompileOptions) {
    drop(Box::from_raw(aOpts as *mut CompileOptions));
}

/// NUL-terminated source (QuickJS requires it), padded with newlines so that QuickJS's line
/// numbers match the starting line SpiderMonkey would have used.
unsafe fn source_buffer(lineno: u32, units: *const u8, len: usize) -> Vec<u8> {
    let pad = lineno.saturating_sub(1) as usize;
    let mut buf = Vec::with_capacity(pad + len + 1);
    buf.resize(pad, b'\n');
    if len > 0 {
        buf.extend_from_slice(std::slice::from_raw_parts(units, len));
    }
    buf.push(0);
    buf
}

unsafe fn eval(cx: &mut Cx, options: &CompileOptions, src: &[u8], flags: i32) -> q::JSValue {
    q::JS_Eval(
        cx.qctx(),
        src.as_ptr() as *const c_char,
        src.len() - 1,
        options.filename.as_ptr(),
        flags,
    )
}

pub(crate) unsafe fn Evaluate2(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<root::mozilla::Utf8Unit>, rval: root::JS::MutableHandle<root::JS::Value>) -> bool {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = source_buffer(options.base.lineno, (*srcBuf).units_ as *const u8, (*srcBuf).length_ as usize);
    let result = eval(cx, options, &src, q::JS_EVAL_TYPE_GLOBAL as i32);
    if result.tag() == q::JS_TAG_EXCEPTION {
        return false;
    }
    *rval.ptr = cx.adopt(result);
    true
}

pub(crate) unsafe fn Compile1(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<root::mozilla::Utf8Unit>) -> *mut root::JSScript {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = source_buffer(options.base.lineno, (*srcBuf).units_ as *const u8, (*srcBuf).length_ as usize);
    let flags = (q::JS_EVAL_TYPE_GLOBAL | q::JS_EVAL_FLAG_COMPILE_ONLY) as i32;
    let result = eval(cx, options, &src, flags);
    if result.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    cx.adopt(result).asBits_ as usize as *mut root::JSScript
}

pub(crate) unsafe fn JS_ExecuteScript(cx: *mut root::JSContext, script: root::JS::Handle<*mut root::JSScript>, rval: root::JS::MutableHandle<root::JS::Value>) -> bool {
    let cx = Cx::from_raw(cx);
    let fun = q::JSValue::mkptr(q::JS_TAG_FUNCTION_BYTECODE, *script.ptr as *mut _);
    // JS_EvalFunction consumes a reference; the script stays retained by the pool.
    let result = q::JS_EvalFunction(cx.qctx(), q::JS_DupValueRT(cx.rt, fun));
    if result.tag() == q::JS_TAG_EXCEPTION {
        return false;
    }
    *rval.ptr = cx.adopt(result);
    true
}

#[allow(dead_code)]
fn _assert_layout(v: JSVal) -> q::JSValue {
    v.to_qjs()
}
