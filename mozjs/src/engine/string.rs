/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Strings, encodings, value conversions, property keys and JSON.
//!
//! A `*mut JSString` is a flat QuickJS string, Latin-1 or UTF-16, so character accessors return
//! pointers into the string itself that stay valid while it lives. Every string is linear, and
//! an atom's string is the atom itself. External strings are copied and their buffer is released
//! at once. Pinned atoms are kept alive by the pool, like every other value handed to Rust.

use std::ffi::{c_char, c_void, CStr};
use std::{ptr, slice};

use quickjs_sys as q;

use super::Cx;
use crate::sys::glue;
use crate::sys::jsapi as root;
use crate::sys::jsval::StringValue;

const ATOM_TAG_INT: q::JSAtom = 1 << 31;

fn str_val(s: *mut root::JSString) -> q::JSValue {
    q::JSValue::mkptr(q::JS_TAG_STRING, s as *mut c_void)
}

/// The characters of a string, borrowed from the string.
#[derive(Clone, Copy)]
pub(crate) enum Chars<'a> {
    Latin1(&'a [u8]),
    TwoByte(&'a [u16]),
}

impl<'a> Chars<'a> {
    /// The characters of the flat string `s`; valid while `s` is alive.
    pub unsafe fn of(s: *mut root::JSString) -> Chars<'a> {
        let mut data = ptr::null();
        let mut wide = 0;
        let len = q::JS_GetStringData(str_val(s), &mut data, &mut wide);
        if wide != 0 {
            Chars::TwoByte(slice::from_raw_parts(data as *const u16, len))
        } else {
            Chars::Latin1(slice::from_raw_parts(data as *const u8, len))
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Chars::Latin1(c) => c.len(),
            Chars::TwoByte(c) => c.len(),
        }
    }

    pub fn at(&self, i: usize) -> u16 {
        match self {
            Chars::Latin1(c) => c[i] as u16,
            Chars::TwoByte(c) => c[i],
        }
    }

    pub fn units(self) -> impl Iterator<Item = u16> + 'a {
        (0..self.len()).map(move |i| self.at(i))
    }

    pub fn to_utf16(self) -> Vec<u16> {
        match self {
            Chars::Latin1(c) => c.iter().map(|&b| b as u16).collect(),
            Chars::TwoByte(c) => c.to_vec(),
        }
    }

    /// UTF-8, with lone surrogates replaced by U+FFFD.
    pub fn to_utf8(self) -> String {
        match self {
            Chars::Latin1(c) => c.iter().map(|&b| b as char).collect(),
            Chars::TwoByte(c) => String::from_utf16_lossy(c),
        }
    }

    /// Each character truncated to 8 bits (SpiderMonkey's lossy Latin-1 conversion).
    pub fn to_latin1_lossy(self) -> Vec<u8> {
        match self {
            Chars::Latin1(c) => c.to_vec(),
            Chars::TwoByte(c) => c.iter().map(|&u| u as u8).collect(),
        }
    }
}

unsafe fn raw_slice<'a, T>(p: *const T, len: usize) -> &'a [T] {
    if len == 0 {
        &[]
    } else {
        slice::from_raw_parts(p, len)
    }
}

unsafe fn range_slice<'a, T>(r: &root::mozilla::Range<T>) -> &'a [T] {
    let start = r.mStart.mPtr;
    let len = (r.mEnd.mPtr as usize - start as usize) / std::mem::size_of::<T>();
    raw_slice(start, len)
}

unsafe fn u16_strlen(s: *const u16) -> usize {
    let mut n = 0;
    while *s.add(n) != 0 {
        n += 1;
    }
    n
}

/// Hands a new string reference (or `JS_EXCEPTION`) to Rust.
unsafe fn adopt_string(cx: &mut Cx, v: q::JSValue) -> *mut root::JSString {
    if v.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    cx.adopt(v).to_string()
}

pub(crate) unsafe fn new_latin1_string(cx: &mut Cx, chars: &[u8]) -> *mut root::JSString {
    adopt_string(cx, q::JS_NewStringLatin1(cx.qctx(), chars.as_ptr(), chars.len()))
}

pub(crate) unsafe fn new_utf16_string(cx: &mut Cx, chars: &[u16]) -> *mut root::JSString {
    adopt_string(cx, q::JS_NewStringUTF16(cx.qctx(), chars.as_ptr(), chars.len()))
}

/// A string from UTF-8; malformed input throws, like SpiderMonkey.
unsafe fn new_utf8_string(cx: &mut Cx, bytes: &[u8]) -> *mut root::JSString {
    if std::str::from_utf8(bytes).is_err() {
        q::JS_ThrowTypeError(cx.qctx(), c"malformed UTF-8 character sequence".as_ptr());
        return ptr::null_mut();
    }
    adopt_string(cx, q::JS_NewStringLen(cx.qctx(), bytes.as_ptr() as *const c_char, bytes.len()))
}

/// A NUL-terminated copy in the runtime's heap (SpiderMonkey's `UniqueChars` and friends, freed
/// by `JS_free`).
unsafe fn malloc_z<T: Copy + Default>(cx: *mut root::JSContext, data: &[T]) -> *mut T {
    let p = q::js_malloc(Cx::from_raw(cx).qctx(), (data.len() + 1) * std::mem::size_of::<T>()) as *mut T;
    if !p.is_null() {
        ptr::copy_nonoverlapping(data.as_ptr(), p, data.len());
        *p.add(data.len()) = T::default();
    }
    p
}

fn ranged_ptr<T>(p: *mut T) -> root::mozilla::RangedPtr<T> {
    root::mozilla::RangedPtr { _phantom_0: std::marker::PhantomData, mPtr: p }
}

// Characters.

pub(crate) unsafe fn JS_GetStringLength(str_: *mut root::JSString) -> usize {
    Chars::of(str_).len()
}

pub(crate) unsafe fn JS_DeprecatedStringHasLatin1Chars(str_: *mut root::JSString) -> bool {
    matches!(Chars::of(str_), Chars::Latin1(_))
}

pub(crate) unsafe fn JS_GetLatin1StringCharsAndLength(cx: *mut root::JSContext, nogc: *const root::JS::AutoRequireNoGC, str_: *mut root::JSString, length: *mut usize) -> *const root::JS::Latin1Char {
    let _ = (cx, nogc);
    match Chars::of(str_) {
        Chars::Latin1(c) => {
            *length = c.len();
            c.as_ptr()
        },
        Chars::TwoByte(_) => panic!("JS_GetLatin1StringCharsAndLength on a two-byte string"),
    }
}

pub(crate) unsafe fn JS_GetTwoByteStringCharsAndLength(cx: *mut root::JSContext, nogc: *const root::JS::AutoRequireNoGC, str_: *mut root::JSString, length: *mut usize) -> *const u16 {
    let _ = (cx, nogc);
    match Chars::of(str_) {
        Chars::TwoByte(c) => {
            *length = c.len();
            c.as_ptr()
        },
        Chars::Latin1(_) => panic!("JS_GetTwoByteStringCharsAndLength on a Latin-1 string"),
    }
}

pub(crate) unsafe fn JS_GetStringCharAt(cx: *mut root::JSContext, str_: *mut root::JSString, index: usize, res: *mut u16) -> bool {
    let _ = cx;
    *res = Chars::of(str_).at(index);
    true
}

pub(crate) unsafe fn JS_StringIsLinear(str_: *mut root::JSString) -> bool {
    let _ = str_;
    true
}

pub(crate) unsafe fn JS_EnsureLinearString(cx: *mut root::JSContext, str_: *mut root::JSString) -> *mut root::JSLinearString {
    let _ = cx;
    str_ as *mut root::JSLinearString
}

pub(crate) unsafe fn StringToLinearStringSlow(cx: *mut root::JSContext, str_: *mut root::JSString) -> *mut root::JSLinearString {
    let _ = cx;
    str_ as *mut root::JSLinearString
}

pub(crate) unsafe fn JS_ForgetStringLinearness(str_: *mut root::JSLinearString) -> *mut root::JSString {
    str_ as *mut root::JSString
}

pub(crate) unsafe fn AtomToLinearString(atom: *mut root::JSAtom) -> *mut root::JSLinearString {
    atom as *mut root::JSLinearString
}

pub(crate) unsafe fn GetLinearStringLength(s: *mut root::JSLinearString) -> usize {
    Chars::of(s as *mut root::JSString).len()
}

pub(crate) unsafe fn GetLinearStringCharAt(s: *mut root::JSLinearString, idx: usize) -> u16 {
    Chars::of(s as *mut root::JSString).at(idx)
}

pub(crate) unsafe fn JS_CopyStringChars(cx: *mut root::JSContext, dest: *const root::mozilla::Range<u16>, str_: *mut root::JSString) -> bool {
    let _ = cx;
    let chars = Chars::of(str_);
    let dest = (*dest).mStart.mPtr;
    for (i, c) in chars.units().enumerate() {
        *dest.add(i) = c;
    }
    true
}

// Comparisons.

pub(crate) unsafe fn JS_CompareStrings(cx: *mut root::JSContext, str1: *mut root::JSString, str2: *mut root::JSString, result: *mut i32) -> bool {
    let _ = cx;
    *result = Chars::of(str1).units().cmp(Chars::of(str2).units()) as i32;
    true
}

fn equals_ascii(chars: Chars, ascii: &[u8]) -> bool {
    chars.len() == ascii.len() && chars.units().zip(ascii).all(|(c, &b)| c == b as u16)
}

pub(crate) unsafe fn JS_StringEqualsAscii(cx: *mut root::JSContext, str_: *mut root::JSString, asciiBytes: *const ::std::os::raw::c_char, match_: *mut bool) -> bool {
    let _ = cx;
    *match_ = equals_ascii(Chars::of(str_), CStr::from_ptr(asciiBytes).to_bytes());
    true
}

pub(crate) unsafe fn JS_StringEqualsAscii1(cx: *mut root::JSContext, str_: *mut root::JSString, asciiBytes: *const ::std::os::raw::c_char, length: usize, match_: *mut bool) -> bool {
    let _ = cx;
    *match_ = equals_ascii(Chars::of(str_), raw_slice(asciiBytes as *const u8, length));
    true
}

pub(crate) unsafe fn JS_LinearStringEqualsAscii(str_: *mut root::JSLinearString, asciiBytes: *const ::std::os::raw::c_char) -> bool {
    equals_ascii(Chars::of(str_ as *mut root::JSString), CStr::from_ptr(asciiBytes).to_bytes())
}

pub(crate) unsafe fn JS_LinearStringEqualsAscii1(str_: *mut root::JSLinearString, asciiBytes: *const ::std::os::raw::c_char, length: usize) -> bool {
    equals_ascii(Chars::of(str_ as *mut root::JSString), raw_slice(asciiBytes as *const u8, length))
}

// Creation.

pub(crate) unsafe fn JS_NewStringCopyN(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char, n: usize) -> *mut root::JSString {
    new_latin1_string(Cx::from_raw(cx), raw_slice(s as *const u8, n))
}

pub(crate) unsafe fn JS_NewStringCopyZ(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    if s.is_null() {
        return new_latin1_string(cx, &[]);
    }
    new_latin1_string(cx, CStr::from_ptr(s).to_bytes())
}

pub(crate) unsafe fn JS_NewUCStringCopyN(cx: *mut root::JSContext, s: *const u16, n: usize) -> *mut root::JSString {
    new_utf16_string(Cx::from_raw(cx), raw_slice(s, n))
}

pub(crate) unsafe fn JS_NewUCStringCopyZ(cx: *mut root::JSContext, s: *const u16) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    if s.is_null() {
        return new_utf16_string(cx, &[]);
    }
    new_utf16_string(cx, raw_slice(s, u16_strlen(s)))
}

pub(crate) unsafe fn JS_NewStringCopyUTF8N(cx: *mut root::JSContext, s: *const root::JS::UTF8Chars) -> *mut root::JSString {
    new_utf8_string(Cx::from_raw(cx), range_slice(&(*s)._base))
}

pub(crate) unsafe fn JS_NewStringCopyUTF8Z(cx: *mut root::JSContext, s: root::JS::ConstUTF8CharsZ) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    if s.data_.is_null() {
        return new_latin1_string(cx, &[]);
    }
    new_utf8_string(cx, CStr::from_ptr(s.data_).to_bytes())
}

pub(crate) unsafe fn JS_NewDependentString(cx: *mut root::JSContext, str_: root::JS::Handle<*mut root::JSString>, start: usize, length: usize) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    let range = start..start + length;
    match Chars::of(*str_.ptr) {
        Chars::Latin1(c) => new_latin1_string(cx, &c[range]),
        Chars::TwoByte(c) => new_utf16_string(cx, &c[range]),
    }
}

pub(crate) unsafe fn JS_ConcatStrings(cx: *mut root::JSContext, left: root::JS::Handle<*mut root::JSString>, right: root::JS::Handle<*mut root::JSString>) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    match (Chars::of(*left.ptr), Chars::of(*right.ptr)) {
        (Chars::Latin1(l), Chars::Latin1(r)) => new_latin1_string(cx, &[l, r].concat()),
        (l, r) => {
            let mut chars = l.to_utf16();
            chars.extend(r.units());
            new_utf16_string(cx, &chars)
        },
    }
}

pub(crate) unsafe fn JS_GetEmptyString(cx: *mut root::JSContext) -> *mut root::JSString {
    new_latin1_string(Cx::from_raw(cx), &[])
}

pub(crate) unsafe fn JS_GetEmptyStringValue(cx: *mut root::JSContext, dest: *mut root::JS::Value) {
    *dest = StringValue(&*JS_GetEmptyString(cx));
}

// External strings: QuickJS has none, so the characters are copied and released at once.

#[repr(C)]
struct ExternalStringCallbacks {
    /// Where SpiderMonkey's `JSExternalStringCallbacks` has its vtable; unused.
    vtable: *const c_void,
    traps: glue::JSExternalStringCallbacksTraps,
    private: *mut c_void,
}

pub(crate) unsafe fn CreateJSExternalStringCallbacks(aTraps: *const glue::JSExternalStringCallbacksTraps, privateData: *mut ::std::os::raw::c_void) -> *mut root::JSExternalStringCallbacks {
    let callbacks = Box::new(ExternalStringCallbacks {
        vtable: ptr::null(),
        traps: *aTraps,
        private: privateData,
    });
    Box::into_raw(callbacks) as *mut root::JSExternalStringCallbacks
}

pub(crate) unsafe fn DeleteJSExternalStringCallbacks(callbacks: *mut root::JSExternalStringCallbacks) {
    drop(Box::from_raw(callbacks as *mut ExternalStringCallbacks));
}

pub(crate) unsafe fn JS_NewExternalStringLatin1(cx: *mut root::JSContext, chars: *const root::JS::Latin1Char, length: usize, callbacks: *const root::JSExternalStringCallbacks) -> *mut root::JSString {
    let s = new_latin1_string(Cx::from_raw(cx), raw_slice(chars, length));
    if !s.is_null() {
        let callbacks = &*(callbacks as *const ExternalStringCallbacks);
        if let Some(finalize) = callbacks.traps.latin1Finalize {
            finalize(callbacks.private, chars as *mut _);
        }
    }
    s
}

pub(crate) unsafe fn JS_NewExternalUCString(cx: *mut root::JSContext, chars: *const u16, length: usize, callbacks: *const root::JSExternalStringCallbacks) -> *mut root::JSString {
    let s = new_utf16_string(Cx::from_raw(cx), raw_slice(chars, length));
    if !s.is_null() {
        let callbacks = &*(callbacks as *const ExternalStringCallbacks);
        if let Some(finalize) = callbacks.traps.utf16Finalize {
            finalize(callbacks.private, chars as *mut _);
        }
    }
    s
}

pub(crate) unsafe fn JS_NewMaybeExternalStringLatin1(cx: *mut root::JSContext, chars: *const root::JS::Latin1Char, length: usize, callbacks: *const root::JSExternalStringCallbacks, allocatedExternal: *mut bool) -> *mut root::JSString {
    let _ = callbacks;
    *allocatedExternal = false;
    new_latin1_string(Cx::from_raw(cx), raw_slice(chars, length))
}

pub(crate) unsafe fn JS_NewMaybeExternalUCString(cx: *mut root::JSContext, chars: *const u16, length: usize, callbacks: *const root::JSExternalStringCallbacks, allocatedExternal: *mut bool) -> *mut root::JSString {
    let _ = callbacks;
    *allocatedExternal = false;
    new_utf16_string(Cx::from_raw(cx), raw_slice(chars, length))
}

pub(crate) unsafe fn JS_NewMaybeExternalStringUTF8(cx: *mut root::JSContext, utf8: *const root::JS::UTF8Chars, callbacks: *const root::JSExternalStringCallbacks, allocatedExternal: *mut bool) -> *mut root::JSString {
    let _ = callbacks;
    *allocatedExternal = false;
    JS_NewStringCopyUTF8N(cx, utf8)
}

// Atoms.

/// The atom of the new string reference `s` (consumed). Integer-like strings have no atom string
/// (QuickJS makes them integer atoms) and are returned themselves.
unsafe fn atomize(cx: &mut Cx, s: q::JSValue) -> *mut root::JSString {
    if s.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    let atom = q::JS_ValueToAtom(cx.qctx(), s);
    if atom & ATOM_TAG_INT != 0 {
        return adopt_string(cx, s);
    }
    q::JS_FreeValueRT(cx.rt, s);
    if atom == q::JS_ATOM_NULL {
        return ptr::null_mut();
    }
    super::atom_to_string_ptr(cx.adopt_atom(atom))
}

unsafe fn atomize_latin1(cx: *mut root::JSContext, chars: &[u8]) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    let s = q::JS_NewStringLatin1(cx.qctx(), chars.as_ptr(), chars.len());
    atomize(cx, s)
}

unsafe fn atomize_utf16(cx: *mut root::JSContext, chars: &[u16]) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    let s = q::JS_NewStringUTF16(cx.qctx(), chars.as_ptr(), chars.len());
    atomize(cx, s)
}

pub(crate) unsafe fn JS_AtomizeString(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char) -> *mut root::JSString {
    atomize_latin1(cx, CStr::from_ptr(s).to_bytes())
}

pub(crate) unsafe fn JS_AtomizeStringN(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char, length: usize) -> *mut root::JSString {
    atomize_latin1(cx, raw_slice(s as *const u8, length))
}

pub(crate) unsafe fn JS_AtomizeAndPinString(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char) -> *mut root::JSString {
    atomize_latin1(cx, CStr::from_ptr(s).to_bytes())
}

pub(crate) unsafe fn JS_AtomizeAndPinStringN(cx: *mut root::JSContext, s: *const ::std::os::raw::c_char, length: usize) -> *mut root::JSString {
    atomize_latin1(cx, raw_slice(s as *const u8, length))
}

pub(crate) unsafe fn JS_AtomizeUCString(cx: *mut root::JSContext, s: *const u16) -> *mut root::JSString {
    atomize_utf16(cx, raw_slice(s, u16_strlen(s)))
}

pub(crate) unsafe fn JS_AtomizeUCStringN(cx: *mut root::JSContext, s: *const u16, length: usize) -> *mut root::JSString {
    atomize_utf16(cx, raw_slice(s, length))
}

// Encodings.

pub(crate) unsafe fn EncodeStringToUTF8(cx: *mut root::JSContext, str_: root::JS::HandleString, cb: glue::EncodedStringCallback) {
    let _ = cx;
    let mut bytes = Chars::of(*str_.ptr).to_utf8().into_bytes();
    bytes.push(0);
    cb(bytes.as_ptr() as *const c_char);
}

pub(crate) unsafe fn JS_EncodeStringToUTF8(cx: *mut root::JSContext, str_: root::JS::Handle<*mut root::JSString>) -> root::JS::UniqueChars {
    malloc_z(cx, Chars::of(*str_.ptr).to_utf8().as_bytes()) as root::JS::UniqueChars
}

pub(crate) unsafe fn JS_EncodeStringToLatin1(cx: *mut root::JSContext, str_: *mut root::JSString) -> root::JS::UniqueChars {
    malloc_z(cx, &Chars::of(str_).to_latin1_lossy()) as root::JS::UniqueChars
}

pub(crate) unsafe fn JS_EncodeStringToASCII(cx: *mut root::JSContext, str_: *mut root::JSString) -> root::JS::UniqueChars {
    JS_EncodeStringToLatin1(cx, str_)
}

pub(crate) unsafe fn JS_GetStringEncodingLength(cx: *mut root::JSContext, str_: *mut root::JSString) -> usize {
    let _ = cx;
    Chars::of(str_).len()
}

pub(crate) unsafe fn JS_EncodeStringToBuffer(cx: *mut root::JSContext, str_: *mut root::JSString, buffer: *mut ::std::os::raw::c_char, length: usize) -> bool {
    let _ = cx;
    let bytes = Chars::of(str_).to_latin1_lossy();
    ptr::copy_nonoverlapping(bytes.as_ptr(), buffer as *mut u8, bytes.len().min(length));
    true
}

pub(crate) unsafe fn LossyTwoByteCharsToNewLatin1CharsZ(cx: *mut root::JSContext, tbchars: *const root::mozilla::Range<u16>) -> root::JS::Latin1CharsZ {
    let bytes: Vec<u8> = range_slice(&*tbchars).iter().map(|&c| c as u8).collect();
    root::JS::Latin1CharsZ { _base: ranged_ptr(malloc_z(cx, &bytes)) }
}

unsafe fn utf8_to_two_byte_z(cx: *mut root::JSContext, utf8: *const root::JS::UTF8Chars, outlen: *mut usize, lossy: bool) -> root::JS::TwoByteCharsZ {
    let bytes = range_slice(&(*utf8)._base);
    let units: Vec<u16> = match std::str::from_utf8(bytes) {
        Ok(s) => s.encode_utf16().collect(),
        Err(_) if lossy => String::from_utf8_lossy(bytes).encode_utf16().collect(),
        Err(_) => {
            q::JS_ThrowTypeError(Cx::from_raw(cx).qctx(), c"malformed UTF-8 character sequence".as_ptr());
            *outlen = 0;
            return root::JS::TwoByteCharsZ { _base: ranged_ptr(ptr::null_mut()) };
        },
    };
    *outlen = units.len();
    root::JS::TwoByteCharsZ { _base: ranged_ptr(malloc_z(cx, &units)) }
}

pub(crate) unsafe fn UTF8CharsToNewTwoByteCharsZ(cx: *mut root::JSContext, utf8: *const root::JS::UTF8Chars, outlen: *mut usize, destArenaId: root::arena_id_t) -> root::JS::TwoByteCharsZ {
    let _ = destArenaId;
    utf8_to_two_byte_z(cx, utf8, outlen, false)
}

pub(crate) unsafe fn LossyUTF8CharsToNewTwoByteCharsZ(cx: *mut root::JSContext, utf8: *const root::JS::UTF8Chars, outlen: *mut usize, destArenaId: root::arena_id_t) -> root::JS::TwoByteCharsZ {
    let _ = destArenaId;
    utf8_to_two_byte_z(cx, utf8, outlen, true)
}

// The runtime's allocator, so memory can move between these and ArrayBuffer contents.

pub(crate) unsafe fn JS_malloc(cx: *mut root::JSContext, nbytes: usize) -> *mut ::std::os::raw::c_void {
    q::js_malloc(Cx::from_raw(cx).qctx(), nbytes)
}

pub(crate) unsafe fn JS_realloc(cx: *mut root::JSContext, p: *mut ::std::os::raw::c_void, oldBytes: usize, newBytes: usize) -> *mut ::std::os::raw::c_void {
    let _ = oldBytes;
    q::js_realloc(Cx::from_raw(cx).qctx(), p, newBytes)
}

pub(crate) unsafe fn JS_free(cx: *mut root::JSContext, p: *mut ::std::os::raw::c_void) {
    q::js_free_rt(Cx::from_raw(cx).rt, p)
}

/// Appends `chars` escaped like SpiderMonkey's `QuoteString`: printable ASCII as is, the rest as
/// `\n`-style, `\xXX` or `\uXXXX` escapes. `quote` (if not 0) surrounds the result and is escaped.
fn escape_into(out: &mut String, chars: Chars, quote: u8) {
    use std::fmt::Write;
    if quote != 0 {
        out.push(quote as char);
    }
    for c in chars.units() {
        if (0x20..0x7f).contains(&c) && c != quote as u16 && c != b'\\' as u16 {
            out.push(c as u8 as char);
            continue;
        }
        let escape = match c {
            0x08 => Some('b'),
            0x0c => Some('f'),
            0x0a => Some('n'),
            0x0d => Some('r'),
            0x09 => Some('t'),
            0x0b => Some('v'),
            0x22 => Some('"'),
            0x27 => Some('\''),
            0x5c => Some('\\'),
            _ => None,
        };
        let _ = match escape {
            Some(e) => write!(out, "\\{e}"),
            None if c < 0x100 => write!(out, "\\x{c:02X}"),
            None => write!(out, "\\u{c:04X}"),
        };
    }
    if quote != 0 {
        out.push(quote as char);
    }
}

unsafe fn put_escaped(buffer: *mut c_char, size: usize, chars: Chars, quote: c_char) -> usize {
    let mut escaped = String::new();
    escape_into(&mut escaped, chars, quote as u8);
    if !buffer.is_null() && size > 0 {
        let n = escaped.len().min(size - 1);
        ptr::copy_nonoverlapping(escaped.as_ptr(), buffer as *mut u8, n);
        *buffer.add(n) = 0;
    }
    escaped.len()
}

pub(crate) unsafe fn JS_PutEscapedString(cx: *mut root::JSContext, buffer: *mut ::std::os::raw::c_char, size: usize, str_: *mut root::JSString, quote: ::std::os::raw::c_char) -> usize {
    let _ = cx;
    put_escaped(buffer, size, Chars::of(str_), quote)
}

pub(crate) unsafe fn JS_PutEscapedLinearString(buffer: *mut ::std::os::raw::c_char, size: usize, str_: *mut root::JSLinearString, quote: ::std::os::raw::c_char) -> usize {
    put_escaped(buffer, size, Chars::of(str_ as *mut root::JSString), quote)
}
