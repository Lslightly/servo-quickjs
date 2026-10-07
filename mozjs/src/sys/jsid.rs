/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! `jsid` (`JS::PropertyKey`) on QuickJS. Integer ids keep SpiderMonkey's encoding; string and
//! symbol ids hold a QuickJS atom: `(atom << 3) | tag`. Atoms referenced by ids are retained by
//! the shim's pool when the id is created.

#![allow(non_snake_case)]

use crate::sys::jsapi::JS::Symbol;
use crate::sys::jsapi::{jsid, JSString};
use libc::c_void;

#[deprecated]
pub const JSID_VOID: jsid = VoidId();

const JSID_TYPE_MASK: usize = 0x7;
const ATOM_SHIFT: usize = 3;

#[repr(usize)]
enum PropertyKeyTag {
    Int = 0x1,
    String = 0x0,
    Void = 0x2,
    Symbol = 0x4,
}

#[inline(always)]
const fn AsPropertyKey(bits: usize) -> jsid {
    jsid { asBits_: bits }
}

#[inline(always)]
pub const fn VoidId() -> jsid {
    AsPropertyKey(PropertyKeyTag::Void as usize)
}

#[inline(always)]
pub fn IntId(i: i32) -> jsid {
    assert!(i >= 0);
    AsPropertyKey((((i as u32) << 1) as usize) | (PropertyKeyTag::Int as usize))
}

#[inline(always)]
pub fn SymbolId(symbol: *mut Symbol) -> jsid {
    assert!(!symbol.is_null());
    from_atom(unsafe { crate::engine::symbol_ptr_to_atom(symbol) })
}

#[inline(always)]
pub fn StringId(s: *mut JSString) -> jsid {
    assert!(!s.is_null());
    from_atom(unsafe { crate::engine::string_ptr_to_atom(s) })
}

/// The id for a QuickJS atom (no reference count change).
pub(crate) fn from_atom(atom: u32) -> jsid {
    if atom & (1 << 31) != 0 {
        return IntId((atom & 0x7fff_ffff) as i32);
    }
    let tag = if unsafe { crate::engine::atom_is_symbol(atom) } {
        PropertyKeyTag::Symbol
    } else {
        PropertyKeyTag::String
    };
    AsPropertyKey(((atom as usize) << ATOM_SHIFT) | tag as usize)
}

impl jsid {
    #[inline(always)]
    fn asBits(&self) -> usize {
        self.asBits_
    }

    /// The QuickJS atom of this id (no reference count change; `JS_ATOM_NULL` for void).
    pub(crate) fn to_atom(&self) -> u32 {
        if self.is_int() {
            (self.to_int() as u32) | (1 << 31)
        } else if self.is_void() {
            0
        } else {
            (self.asBits() >> ATOM_SHIFT) as u32
        }
    }

    #[inline(always)]
    pub fn is_void(&self) -> bool {
        self.asBits() == (PropertyKeyTag::Void as usize)
    }

    #[inline(always)]
    pub fn is_int(&self) -> bool {
        (self.asBits() & (PropertyKeyTag::Int as usize)) != 0
    }

    #[inline(always)]
    pub fn is_string(&self) -> bool {
        (self.asBits() & JSID_TYPE_MASK) == (PropertyKeyTag::String as usize)
    }

    #[inline(always)]
    pub fn is_symbol(&self) -> bool {
        (self.asBits() & JSID_TYPE_MASK) == (PropertyKeyTag::Symbol as usize)
    }

    #[inline(always)]
    pub fn is_gcthing(&self) -> bool {
        self.is_string() || self.is_symbol()
    }

    #[inline(always)]
    pub fn to_int(&self) -> i32 {
        assert!(self.is_int());
        ((self.asBits() as u32) >> 1) as i32
    }

    #[inline(always)]
    pub fn to_string(&self) -> *mut JSString {
        assert!(self.is_string());
        unsafe { crate::engine::atom_to_string_ptr(self.to_atom()) }
    }

    #[inline(always)]
    pub fn to_symbol(&self) -> *mut Symbol {
        assert!(self.is_symbol());
        unsafe { crate::engine::atom_to_symbol_ptr(self.to_atom()) }
    }

    #[inline(always)]
    pub fn to_gcthing(&self) -> *mut c_void {
        assert!(self.is_gcthing());
        if self.is_string() {
            self.to_string() as *mut c_void
        } else {
            self.to_symbol() as *mut c_void
        }
    }
}
