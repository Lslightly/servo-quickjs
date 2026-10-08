/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Conversions between QuickJS atoms and the string/symbol pointers SpiderMonkey ids expose.
//! A QuickJS atom is a `JSString` itself (`rt->atom_array[atom]`), so the pointers are stable for
//! as long as the atom lives.

use servo_quickjs as q;

use super::current;
use crate::sys::jsapi as root;

pub(crate) unsafe fn atom_to_string_ptr(atom: q::JSAtom) -> *mut root::JSString {
    let cx = current();
    let v = q::JS_AtomToString(cx.base_ctx, atom);
    assert_eq!(v.tag(), q::JS_TAG_STRING);
    let p = v.ptr();
    // The atom keeps the string alive.
    q::JS_FreeValueRT(cx.rt, v);
    p as *mut root::JSString
}

pub(crate) unsafe fn atom_to_symbol_ptr(atom: q::JSAtom) -> *mut root::JS::Symbol {
    let cx = current();
    let v = q::JS_AtomToValue(cx.base_ctx, atom);
    assert_eq!(v.tag(), q::JS_TAG_SYMBOL);
    let p = v.ptr();
    q::JS_FreeValueRT(cx.rt, v);
    p as *mut root::JS::Symbol
}

pub(crate) unsafe fn atom_is_symbol(atom: q::JSAtom) -> bool {
    let cx = current();
    let v = q::JS_AtomToValue(cx.base_ctx, atom);
    let is_symbol = v.tag() == q::JS_TAG_SYMBOL;
    q::JS_FreeValueRT(cx.rt, v);
    is_symbol
}

/// The atom of a string; retained by the pool.
pub(crate) unsafe fn string_ptr_to_atom(s: *mut root::JSString) -> q::JSAtom {
    let cx = current();
    let atom = q::JS_ValueToAtom(
        cx.base_ctx,
        q::JSValue::mkptr(q::JS_TAG_STRING, s as *mut _),
    );
    cx.adopt_atom(atom)
}

/// The atom of a symbol; retained by the pool.
pub(crate) unsafe fn symbol_ptr_to_atom(s: *mut root::JS::Symbol) -> q::JSAtom {
    let cx = current();
    let atom = q::JS_ValueToAtom(
        cx.base_ctx,
        q::JSValue::mkptr(q::JS_TAG_SYMBOL, s as *mut _),
    );
    cx.adopt_atom(atom)
}
