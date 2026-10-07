/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! QuickJS implementations of SpiderMonkey API functions.
//!
//! Every `pub(crate) unsafe fn NAME` in this module tree replaces the stub of the SpiderMonkey
//! function `NAME` (see `tools/stub_bindings.py`), so signatures must match the bindgen ones
//! (`tools/sig.py NAME` prints them). Design: `docs/design.md`.

#![allow(non_snake_case)]

use std::cell::Cell;
use std::ffi::CString;
use std::ptr;

use quickjs_sys as q;

use crate::sys::jsapi as root;
use crate::sys::jsval::JSVal;

mod atom;
mod class;
mod eval;
mod exception;
mod property;
mod realm;
mod runtime;

pub(crate) use atom::*;
pub(crate) use class::*;
pub(crate) use eval::*;
pub(crate) use exception::*;
pub(crate) use property::*;
pub(crate) use realm::*;
pub(crate) use runtime::*;

/// What a SpiderMonkey `JSContext*` points to: one per QuickJS runtime (one per thread).
pub(crate) struct Cx {
    pub rt: *mut q::JSRuntime,
    /// A context without a global of ours, for operations outside any realm.
    pub base_ctx: *mut q::JSContext,
    /// Entered realms, innermost last.
    pub realm_stack: Vec<*mut RealmData>,
    /// Values (and atoms) handed to Rust during the current task; see `retain`.
    pub pool: Vec<q::JSValue>,
    pub atom_pool: Vec<q::JSAtom>,
    pub warning_reporter: root::JS::WarningReporter,
}

/// What a SpiderMonkey `JS::Realm*` (and `JS::Compartment*`) points to.
pub(crate) struct RealmData {
    pub qctx: *mut q::JSContext,
    pub global: *mut root::JSObject,
    pub private: *mut std::os::raw::c_void,
    pub principals: *mut root::JSPrincipals,
    /// Reserved slots of the global (QuickJS's global object has no opaque of ours yet).
    pub global_data: Option<Box<class::ObjectData>>,
}

thread_local!(static CURRENT: Cell<*mut Cx> = const { Cell::new(ptr::null_mut()) });

impl Cx {
    pub fn from_raw<'a>(cx: *mut root::JSContext) -> &'a mut Cx {
        assert!(!cx.is_null());
        unsafe { &mut *(cx as *mut Cx) }
    }

    pub fn as_raw(&mut self) -> *mut root::JSContext {
        self as *mut Cx as *mut root::JSContext
    }

    /// The QuickJS context of the current realm, or the base context outside any realm.
    pub fn qctx(&self) -> *mut q::JSContext {
        match self.realm_stack.last() {
            Some(realm) => unsafe { (**realm).qctx },
            None => self.base_ctx,
        }
    }

    pub fn current_realm(&self) -> *mut RealmData {
        self.realm_stack.last().copied().unwrap_or(ptr::null_mut())
    }

    /// Keeps `v` alive until the pool is drained (end of task), and returns it. Ropes are
    /// flattened, so values Rust sees never carry `JS_TAG_STRING_ROPE`.
    pub fn retain(&mut self, v: JSVal) -> JSVal {
        let qv = v.to_qjs();
        if !qv.has_ref_count() {
            return v;
        }
        let qv = unsafe { q::JS_DupValueRT(self.rt, qv) };
        self.adopt(qv)
    }

    /// Like `retain`, but takes over a reference the caller owns (e.g. a QuickJS API result).
    pub fn adopt(&mut self, qv: q::JSValue) -> JSVal {
        if !qv.has_ref_count() {
            return JSVal::from_qjs(qv);
        }
        let qv = if qv.tag() == q::JS_TAG_STRING_ROPE {
            let flat = unsafe { q::JS_ToString(self.qctx(), qv) };
            unsafe { q::JS_FreeValueRT(self.rt, qv) };
            flat
        } else {
            qv
        };
        self.pool.push(qv);
        JSVal::from_qjs(qv)
    }

    pub fn adopt_atom(&mut self, atom: q::JSAtom) -> q::JSAtom {
        self.atom_pool.push(atom);
        atom
    }

    /// Releases the values retained since `mark` (see `pool_mark`).
    pub fn drain_pool(&mut self, mark: (usize, usize)) {
        while self.pool.len() > mark.0 {
            let v = self.pool.pop().unwrap();
            unsafe { q::JS_FreeValueRT(self.rt, v) };
        }
        while self.atom_pool.len() > mark.1 {
            let a = self.atom_pool.pop().unwrap();
            unsafe { q::JS_FreeAtomRT(self.rt, a) };
        }
    }

    pub fn pool_mark(&self) -> (usize, usize) {
        (self.pool.len(), self.atom_pool.len())
    }
}

/// The runtime of the current thread.
pub(crate) fn current<'a>() -> &'a mut Cx {
    let cx = CURRENT.with(|c| c.get());
    assert!(!cx.is_null(), "no QuickJS runtime on this thread");
    unsafe { &mut *cx }
}

pub(crate) fn set_current(cx: *mut Cx) {
    CURRENT.with(|c| c.set(cx));
}

/// The QuickJS object pointer as a value (no reference count change).
pub(crate) fn obj_val(obj: *mut root::JSObject) -> q::JSValue {
    q::JSValue::mkptr(q::JS_TAG_OBJECT, obj as *mut _)
}

pub(crate) fn val_obj(v: q::JSValue) -> *mut root::JSObject {
    debug_assert_eq!(v.tag(), q::JS_TAG_OBJECT);
    v.ptr() as *mut root::JSObject
}

pub(crate) fn cstring_lossy(s: *const std::os::raw::c_char) -> CString {
    if s.is_null() {
        CString::default()
    } else {
        unsafe { std::ffi::CStr::from_ptr(s) }.to_owned()
    }
}

/// Retains the initial value of a `JS::Rooted<T>` (see `JS::Rooted::add_to_root_stack`).
pub(crate) unsafe fn retain_rooted(cx: *mut root::JSContext, kind: root::JS::RootKind, data: *const std::ffi::c_void) {
    use root::JS::RootKind;
    let cx = Cx::from_raw(cx);
    let ptr_tag = match kind {
        RootKind::Value => {
            cx.retain(*(data as *const JSVal));
            return;
        },
        RootKind::Id => {
            let id = *(data as *const root::jsid);
            if id.is_string() || id.is_symbol() {
                cx.adopt_atom(q::JS_DupAtom(cx.base_ctx, id.to_atom()));
            }
            return;
        },
        RootKind::Object => q::JS_TAG_OBJECT,
        RootKind::String => q::JS_TAG_STRING,
        RootKind::Symbol => q::JS_TAG_SYMBOL,
        RootKind::BigInt => q::JS_TAG_BIG_INT,
        RootKind::Script => q::JS_TAG_FUNCTION_BYTECODE,
        _ => return,
    };
    let p = *(data as *const *mut std::ffi::c_void);
    if !p.is_null() {
        cx.retain(JSVal::from_qjs(q::JSValue::mkptr(ptr_tag, p)));
    }
}
