/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Arrays, id vectors and enumeration, Map/Set/WeakMap, Date, RegExp, functions with reserved
//! slots, iteration and the remaining property overloads.
//!
//! A `JS::StackGCVector<T>` is a Rust `Vec<T>`: a `Handle<StackGCVector<T>>` points to a `Vec<T>`,
//! and the `PersistentRooted*Vector`s are boxed `Vec`s. Entries hold no reference count; the GC
//! things they refer to are retained by the pool, like every other value handed to Rust.
//!
//! Builtins without a QuickJS C API (Map, Set, WeakMap, Date, RegExp) call the methods of the
//! current realm's intrinsic prototypes.

use std::collections::HashSet;
use std::ffi::c_int;
use std::ptr;

use quickjs_sys as q;

use super::class::{adopt_object, object_data, QJS_CLASS_BYTECODE_FUNCTION, QJS_CLASS_OBJECT};
use super::property::{data_flags, Atom};
use super::{obj_val, Cx};
use crate::sys::jsapi as root;
use crate::sys::jsval::JSVal;

// QuickJS's internal class ids (`enum { JS_CLASS_OBJECT = 1, ... }` in quickjs.c).
const CLASS_ARRAY: q::JSClassID = 2;
const CLASS_ERROR: q::JSClassID = 3;
const CLASS_NUMBER: q::JSClassID = 4;
const CLASS_STRING: q::JSClassID = 5;
const CLASS_BOOLEAN: q::JSClassID = 6;
const CLASS_ARGUMENTS: q::JSClassID = 8;
const CLASS_MAPPED_ARGUMENTS: q::JSClassID = 9;
const CLASS_DATE: q::JSClassID = 10;
const CLASS_REGEXP: q::JSClassID = 18;
const CLASS_ARRAY_BUFFER: q::JSClassID = 19;
const CLASS_SHARED_ARRAY_BUFFER: q::JSClassID = 20;
const CLASS_BIG_INT: q::JSClassID = 34;
const CLASS_MAP: q::JSClassID = 35;
const CLASS_SET: q::JSClassID = 36;
const CLASS_ITERATOR: q::JSClassID = 39;
const CLASS_MAP_ITERATOR: q::JSClassID = 43;
const CLASS_SET_ITERATOR: q::JSClassID = 44;
const CLASS_PROXY: q::JSClassID = 51;
const CLASS_PROMISE: q::JSClassID = 52;

fn ok(status: c_int) -> bool {
    status >= 0
}

fn is_exception(v: q::JSValue) -> bool {
    v.tag() == q::JS_TAG_EXCEPTION
}

/// Stores a QuickJS result (a new reference) in `out`; false if it is an exception.
unsafe fn set_result(cx: &mut Cx, v: q::JSValue, out: root::JS::MutableHandleValue) -> bool {
    if is_exception(v) {
        return false;
    }
    *out.ptr = cx.adopt(v);
    true
}

/// Maps a QuickJS status to an `ObjectOpResult`: `< 0` exception, `0` failure with `failure`.
unsafe fn op_result(status: c_int, result: *mut root::JS::ObjectOpResult, failure: root::JSErrNum) -> bool {
    if status < 0 {
        return false;
    }
    if status > 0 {
        (*result).succeed();
    } else {
        (*result).fail(failure);
    }
    true
}

unsafe fn class_of(obj: *mut root::JSObject) -> q::JSClassID {
    q::JS_GetClassID(obj_val(obj))
}

unsafe fn free_desc(cx: &Cx, qd: &q::JSPropertyDescriptor) {
    for v in [qd.value, qd.getter, qd.setter] {
        q::JS_FreeValueRT(cx.rt, v);
    }
}

/// Calls `f` with the own property keys of `obj` (`JS_GPN_*` flags) and their enumerability.
unsafe fn for_each_own_key(cx: &mut Cx, obj: q::JSValue, flags: u32, mut f: impl FnMut(&mut Cx, q::JSAtom, bool)) -> bool {
    let qctx = cx.qctx();
    let mut tab: *mut q::JSPropertyEnum = ptr::null_mut();
    let mut len = 0u32;
    if q::JS_GetOwnPropertyNames(qctx, &mut tab, &mut len, obj, flags as c_int) < 0 {
        return false;
    }
    if len > 0 {
        for e in std::slice::from_raw_parts(tab, len as usize) {
            f(cx, e.atom, e.is_enumerable != 0);
        }
    }
    q::JS_FreePropertyEnum(qctx, tab, len);
    true
}

/// The id of `atom`; a string or symbol atom is kept alive by the pool.
unsafe fn retained_id(cx: &mut Cx, atom: q::JSAtom) -> root::jsid {
    let id = crate::sys::jsid::from_atom(atom);
    if id.is_string() || id.is_symbol() {
        cx.adopt_atom(q::JS_DupAtom(cx.base_ctx, atom));
    }
    id
}

unsafe fn class_proto(cx: *mut root::JSContext, class: q::JSClassID) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let proto = q::JS_GetClassProto(cx.qctx(), class);
    adopt_object(cx, proto)
}

// Arrays.

pub(crate) unsafe fn NewArrayObject(cx: *mut root::JSContext, contents: *const root::JS::HandleValueArray) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let arr = q::JS_NewArray(qctx);
    if is_exception(arr) {
        return ptr::null_mut();
    }
    if !contents.is_null() && (*contents).length_ > 0 {
        let values = std::slice::from_raw_parts((*contents).elements_, (*contents).length_);
        for (i, v) in values.iter().enumerate() {
            let v = q::JS_DupValueRT(cx.rt, v.to_qjs());
            if q::JS_DefinePropertyValueUint32(qctx, arr, i as u32, v, q::JS_PROP_C_W_E as c_int) < 0 {
                q::JS_FreeValueRT(cx.rt, arr);
                return ptr::null_mut();
            }
        }
    }
    adopt_object(cx, arr)
}

pub(crate) unsafe fn NewArrayObject1(cx: *mut root::JSContext, length: usize) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let arr = q::JS_NewArray(qctx);
    if is_exception(arr) {
        return ptr::null_mut();
    }
    if length > 0 && q::JS_SetPropertyStr(qctx, arr, c"length".as_ptr(), q::JS_NewInt64(qctx, length as i64)) < 0 {
        q::JS_FreeValueRT(cx.rt, arr);
        return ptr::null_mut();
    }
    adopt_object(cx, arr)
}

pub(crate) unsafe fn IsArrayObject(cx: *mut root::JSContext, value: root::JS::Handle<root::JS::Value>, isArray: *mut bool) -> bool {
    let cx = Cx::from_raw(cx);
    let v = (*value.ptr).to_qjs();
    if v.tag() != q::JS_TAG_OBJECT {
        *isArray = false;
        return true;
    }
    let r = q::JS_IsArray(cx.qctx(), v);
    *isArray = r > 0;
    ok(r)
}

// Id and object vectors.

pub(crate) unsafe fn CreateRootedIdVector(cx: *mut root::JSContext) -> *mut root::JS::PersistentRootedIdVector {
    let _ = cx;
    Box::into_raw(Box::new(Vec::<root::jsid>::new())) as *mut root::JS::PersistentRootedIdVector
}

pub(crate) unsafe fn DestroyRootedIdVector(v: *mut root::JS::PersistentRootedIdVector) {
    drop(Box::from_raw(v as *mut Vec<root::jsid>));
}

pub(crate) unsafe fn GetIdVectorAddress(v: *mut root::JS::PersistentRootedIdVector) -> *mut ::std::os::raw::c_void {
    v as *mut _
}

pub(crate) unsafe fn SliceRootedIdVector(v: *const root::JS::PersistentRootedIdVector, length: *mut usize) -> *const root::jsid {
    let v = &*(v as *const Vec<root::jsid>);
    *length = v.len();
    v.as_ptr()
}

pub(crate) unsafe fn AppendToIdVector(v: root::JS::MutableHandleIdVector, id: root::JS::HandleId) -> bool {
    let id = *id.ptr;
    if id.is_string() || id.is_symbol() {
        let cx = super::current();
        cx.adopt_atom(q::JS_DupAtom(cx.base_ctx, id.to_atom()));
    }
    (*(v.ptr as *mut Vec<root::jsid>)).push(id);
    true
}

pub(crate) unsafe fn GetPropertyKeys(cx: *mut root::JSContext, obj: root::JS::HandleObject, flags: ::std::os::raw::c_uint, props: root::JS::MutableHandleIdVector) -> bool {
    let cx = Cx::from_raw(cx);
    let out = &mut *(props.ptr as *mut Vec<root::jsid>);
    let hidden = flags & root::JSITER_HIDDEN != 0;
    let mut kinds = 0;
    if flags & root::JSITER_SYMBOLSONLY == 0 {
        kinds |= q::JS_GPN_STRING_MASK;
    }
    if flags & (root::JSITER_SYMBOLS | root::JSITER_SYMBOLSONLY) != 0 {
        kinds |= q::JS_GPN_SYMBOL_MASK;
    }
    if flags & root::JSITER_OWNONLY != 0 {
        let enum_only = if hidden { 0 } else { q::JS_GPN_ENUM_ONLY };
        return for_each_own_key(cx, obj_val(*obj.ptr), kinds | enum_only, |cx, atom, _| out.push(retained_id(cx, atom)));
    }
    // for-in order: own keys first, then each prototype's keys that are not shadowed.
    let mut seen = HashSet::new();
    let mut current = q::JS_DupValueRT(cx.rt, obj_val(*obj.ptr));
    loop {
        let listed = for_each_own_key(cx, current, kinds | q::JS_GPN_SET_ENUM, |cx, atom, enumerable| {
            if seen.insert(atom) && (hidden || enumerable) {
                out.push(retained_id(cx, atom));
            }
        });
        let proto = if listed { q::JS_GetPrototype(cx.qctx(), current) } else { q::JS_EXCEPTION };
        q::JS_FreeValueRT(cx.rt, current);
        if is_exception(proto) {
            return false;
        }
        if proto.tag() != q::JS_TAG_OBJECT {
            return true;
        }
        current = proto;
    }
}

pub(crate) unsafe fn CreateRootedObjectVector(aCx: *mut root::JSContext) -> *mut root::JS::PersistentRootedObjectVector {
    let _ = aCx;
    Box::into_raw(Box::new(Vec::<*mut root::JSObject>::new())) as *mut root::JS::PersistentRootedObjectVector
}

pub(crate) unsafe fn DeleteRootedObjectVector(v: *mut root::JS::PersistentRootedObjectVector) {
    drop(Box::from_raw(v as *mut Vec<*mut root::JSObject>));
}

pub(crate) unsafe fn AppendToRootedObjectVector(v: *mut root::JS::PersistentRootedObjectVector, obj: *mut root::JSObject) -> bool {
    if !obj.is_null() {
        super::current().retain(JSVal::from_qjs(obj_val(obj)));
    }
    (*(v as *mut Vec<*mut root::JSObject>)).push(obj);
    true
}

pub(crate) unsafe fn GetObjectVectorAddress(v: *mut root::JS::PersistentRootedObjectVector) -> *mut ::std::os::raw::c_void {
    v as *mut _
}

/// The `Vec<T>` behind a `Handle<StackGCVector<T>>`.
pub(crate) unsafe fn stack_gc_vector<'a, T>(vec: root::JS::Handle<root::JS::StackGCVector<T, root::js::TempAllocPolicy>>) -> &'a Vec<T> {
    &*(vec.ptr as *const Vec<T>)
}

pub(crate) unsafe fn StackGCVectorValueLength(vec: root::JS::Handle< root::JS::StackGCVector<root::JS::Value, root::js::TempAllocPolicy>, >) -> u32 {
    stack_gc_vector(vec).len() as u32
}

pub(crate) unsafe fn StackGCVectorStringLength(vec: root::JS::Handle< root::JS::StackGCVector<*mut root::JSString, root::js::TempAllocPolicy>, >) -> u32 {
    stack_gc_vector(vec).len() as u32
}

pub(crate) unsafe fn StackGCVectorValueAtIndex(vec: root::JS::Handle< root::JS::StackGCVector<root::JS::Value, root::js::TempAllocPolicy>, >, index: u32) -> *const root::JS::Value {
    &stack_gc_vector(vec)[index as usize]
}

pub(crate) unsafe fn StackGCVectorStringAtIndex(vec: root::JS::Handle< root::JS::StackGCVector<*mut root::JSString, root::js::TempAllocPolicy>, >, index: u32) -> *const *mut root::JSString {
    &stack_gc_vector(vec)[index as usize]
}

// Remaining property overloads.

pub(crate) unsafe fn JS_DefineUCProperty2(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, name: *const u16, namelen: usize, value: root::JS::Handle<root::JS::Value>, attrs: ::std::os::raw::c_uint) -> bool {
    let cx = Cx::from_raw(cx);
    let atom = Atom::from_utf16(cx, name, namelen);
    let v = q::JS_DupValueRT(cx.rt, (*value.ptr).to_qjs());
    ok(q::JS_DefinePropertyValue(cx.qctx(), obj_val(*obj.ptr), atom.atom, v, data_flags(attrs)))
}

pub(crate) unsafe fn JS_ForwardGetPropertyTo(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, id: root::JS::Handle<root::jsid>, receiver: root::JS::Handle<root::JS::Value>, vp: root::JS::MutableHandleValue) -> bool {
    let cx = Cx::from_raw(cx);
    let v = q::JS_GetPropertyInternal(cx.qctx(), obj_val(*obj.ptr), (*id.ptr).to_atom(), (*receiver.ptr).to_qjs(), 0);
    set_result(cx, v, vp)
}

pub(crate) unsafe fn JS_ForwardSetPropertyTo(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, id: root::JS::Handle<root::jsid>, v: root::JS::Handle<root::JS::Value>, receiver: root::JS::Handle<root::JS::Value>, result: *mut root::JS::ObjectOpResult) -> bool {
    let cx = Cx::from_raw(cx);
    let v = q::JS_DupValueRT(cx.rt, (*v.ptr).to_qjs());
    let r = q::JS_SetPropertyInternal(cx.qctx(), obj_val(*obj.ptr), (*id.ptr).to_atom(), v, (*receiver.ptr).to_qjs(), 0);
    op_result(r, result, root::JSErrNum::JSMSG_READ_ONLY)
}

/// OrdinarySetWithOwnDescriptor (ES 10.1.9.2) with `ownDesc` (null: no own property).
pub(crate) unsafe fn SetPropertyIgnoringNamedGetter(cx: *mut root::JSContext, obj: root::JS::HandleObject, id: root::JS::HandleId, v: root::JS::HandleValue, receiver: root::JS::HandleValue, ownDesc: *const root::JS::Handle<root::JS::PropertyDescriptor>, result: *mut root::JS::ObjectOpResult) -> bool {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let atom = (*id.ptr).to_atom();
    let v = (*v.ptr).to_qjs();
    let receiver = (*receiver.ptr).to_qjs();
    let desc = if ownDesc.is_null() {
        let proto = q::JS_GetPrototype(qctx, obj_val(*obj.ptr));
        if is_exception(proto) {
            return false;
        }
        if proto.tag() == q::JS_TAG_OBJECT {
            let r = q::JS_SetPropertyInternal(qctx, proto, atom, q::JS_DupValueRT(cx.rt, v), receiver, 0);
            q::JS_FreeValueRT(cx.rt, proto);
            return op_result(r, result, root::JSErrNum::JSMSG_READ_ONLY);
        }
        None
    } else {
        Some(&*(*ownDesc).ptr)
    };
    let is_accessor = desc.is_some_and(|d| d.hasGetter_() || d.hasSetter_());
    if !is_accessor {
        if desc.is_some_and(|d| !d.writable_()) {
            (*result).fail(root::JSErrNum::JSMSG_READ_ONLY);
            return true;
        }
        if receiver.tag() != q::JS_TAG_OBJECT {
            (*result).fail(root::JSErrNum::JSMSG_SET_NON_OBJECT_RECEIVER);
            return true;
        }
        let mut existing: q::JSPropertyDescriptor = std::mem::zeroed();
        let r = q::JS_GetOwnProperty(qctx, &mut existing, receiver, atom);
        if r < 0 {
            return false;
        }
        if r > 0 {
            free_desc(cx, &existing);
            let flags = existing.flags as u32;
            if flags & q::JS_PROP_GETSET != 0 {
                (*result).fail(root::JSErrNum::JSMSG_OVERWRITING_ACCESSOR);
                return true;
            }
            if flags & q::JS_PROP_WRITABLE == 0 {
                (*result).fail(root::JSErrNum::JSMSG_READ_ONLY);
                return true;
            }
            let r = q::JS_DefineProperty(qctx, receiver, atom, v, q::JS_UNDEFINED, q::JS_UNDEFINED, q::JS_PROP_HAS_VALUE as c_int);
            return op_result(r, result, root::JSErrNum::JSMSG_CANT_REDEFINE_PROP);
        }
        let r = q::JS_DefinePropertyValue(qctx, receiver, atom, q::JS_DupValueRT(cx.rt, v), q::JS_PROP_C_W_E as c_int);
        return op_result(r, result, root::JSErrNum::JSMSG_CANT_DEFINE_PROP_OBJECT_NOT_EXTENSIBLE);
    }
    let desc = desc.unwrap();
    if !desc.hasSetter_() || desc.setter_.is_null() {
        (*result).fail(root::JSErrNum::JSMSG_GETTER_ONLY);
        return true;
    }
    let mut argv = [v];
    let r = q::JS_Call(qctx, obj_val(desc.setter_), receiver, 1, argv.as_mut_ptr());
    if is_exception(r) {
        return false;
    }
    q::JS_FreeValueRT(cx.rt, r);
    (*result).succeed();
    true
}

/// Copies the own properties of `src` (with their attributes) to `dst`.
pub(crate) unsafe fn JS_InitializePropertiesFromCompatibleNativeObject(cx: *mut root::JSContext, dst: root::JS::HandleObject, src: root::JS::HandleObject) -> bool {
    let cx = Cx::from_raw(cx);
    let (dst, src) = (obj_val(*dst.ptr), obj_val(*src.ptr));
    let mut failed = false;
    let listed = for_each_own_key(cx, src, q::JS_GPN_STRING_MASK | q::JS_GPN_SYMBOL_MASK, |cx, atom, _| {
        if failed {
            return;
        }
        let qctx = cx.qctx();
        let mut qd: q::JSPropertyDescriptor = std::mem::zeroed();
        let r = q::JS_GetOwnProperty(qctx, &mut qd, src, atom);
        if r <= 0 {
            failed = r < 0;
            return;
        }
        let attrs = qd.flags as u32 & (q::JS_PROP_CONFIGURABLE | q::JS_PROP_ENUMERABLE | q::JS_PROP_WRITABLE);
        let mut flags = attrs | q::JS_PROP_HAS_CONFIGURABLE | q::JS_PROP_HAS_ENUMERABLE | q::JS_PROP_THROW;
        if qd.flags as u32 & q::JS_PROP_GETSET != 0 {
            flags |= q::JS_PROP_HAS_GET | q::JS_PROP_HAS_SET;
        } else {
            flags |= q::JS_PROP_HAS_VALUE | q::JS_PROP_HAS_WRITABLE;
        }
        failed = q::JS_DefineProperty(qctx, dst, atom, qd.value, qd.getter, qd.setter, flags as c_int) < 0;
        free_desc(cx, &qd);
    });
    listed && !failed
}

// Property descriptors.

pub(crate) unsafe fn FromPropertyDescriptor(cx: *mut root::JSContext, desc_: root::JS::Handle<root::JS::PropertyDescriptor>, vp: root::JS::MutableHandleValue) -> bool {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let d = &*desc_.ptr;
    let obj = q::JS_NewObject(qctx);
    if is_exception(obj) {
        return false;
    }
    let object_or_undefined = |o: *mut root::JSObject| if o.is_null() { q::JS_UNDEFINED } else { q::JS_DupValueRT(cx.rt, obj_val(o)) };
    let mut fields = Vec::new();
    if d.hasValue_() {
        fields.push((c"value", q::JS_DupValueRT(cx.rt, d.value_.to_qjs())));
    }
    if d.hasWritable_() {
        fields.push((c"writable", q::JS_NewBool(qctx, d.writable_() as c_int)));
    }
    if d.hasGetter_() {
        fields.push((c"get", object_or_undefined(d.getter_)));
    }
    if d.hasSetter_() {
        fields.push((c"set", object_or_undefined(d.setter_)));
    }
    if d.hasEnumerable_() {
        fields.push((c"enumerable", q::JS_NewBool(qctx, d.enumerable_() as c_int)));
    }
    if d.hasConfigurable_() {
        fields.push((c"configurable", q::JS_NewBool(qctx, d.configurable_() as c_int)));
    }
    let mut defined = true;
    for (name, v) in fields {
        // Consumes `v` even on failure.
        defined &= ok(q::JS_DefinePropertyValueStr(qctx, obj, name.as_ptr(), v, q::JS_PROP_C_W_E as c_int));
    }
    if !defined {
        q::JS_FreeValueRT(cx.rt, obj);
        return false;
    }
    *vp.ptr = cx.adopt(obj);
    true
}

pub(crate) unsafe fn SetDataPropertyDescriptor(desc: root::JS::MutableHandle<root::JS::PropertyDescriptor>, value: root::JS::HandleValue, attrs: u32) {
    let d = &mut *desc.ptr;
    *d = std::mem::zeroed();
    d.set_hasConfigurable_(true);
    d.set_configurable_(attrs & root::JSPROP_PERMANENT as u32 == 0);
    d.set_hasEnumerable_(true);
    d.set_enumerable_(attrs & root::JSPROP_ENUMERATE as u32 != 0);
    d.set_hasWritable_(true);
    d.set_writable_(attrs & root::JSPROP_READONLY as u32 == 0);
    d.set_hasValue_(true);
    d.set_resolving_(attrs & root::JSPROP_RESOLVING != 0);
    d.value_ = super::current().retain(*value.ptr);
}

pub(crate) unsafe fn CallPropertyDescriptorTracer(trc: *mut root::JSTracer, desc: *mut root::JS::PropertyDescriptor) {
    // Descriptor fields are pool-retained, not counted, so they must not be marked.
    let _ = (trc, desc);
}

// Objects and prototypes.

pub(crate) unsafe fn JS_NewObjectWithoutMetadata(cx: *mut root::JSContext, clasp: *const root::JSClass, proto: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    super::JS_NewObjectWithGivenProto(cx, clasp, proto)
}

/// SetIntegrityLevel(obj, frozen).
pub(crate) unsafe fn JS_FreezeObject(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>) -> bool {
    let cx = Cx::from_raw(cx);
    let o = obj_val(*obj.ptr);
    let r = q::JS_PreventExtensions(cx.qctx(), o);
    if r < 0 {
        return false;
    }
    if r == 0 {
        q::JS_ThrowTypeError(cx.qctx(), c"can't freeze object".as_ptr());
        return false;
    }
    let mut failed = false;
    let listed = for_each_own_key(cx, o, q::JS_GPN_STRING_MASK | q::JS_GPN_SYMBOL_MASK, |cx, atom, _| {
        if failed {
            return;
        }
        let qctx = cx.qctx();
        let mut qd: q::JSPropertyDescriptor = std::mem::zeroed();
        let r = q::JS_GetOwnProperty(qctx, &mut qd, o, atom);
        if r <= 0 {
            failed = r < 0;
            return;
        }
        free_desc(cx, &qd);
        let mut flags = q::JS_PROP_HAS_CONFIGURABLE | q::JS_PROP_THROW;
        if qd.flags as u32 & q::JS_PROP_GETSET == 0 {
            flags |= q::JS_PROP_HAS_WRITABLE;
        }
        failed = q::JS_DefineProperty(qctx, o, atom, q::JS_UNDEFINED, q::JS_UNDEFINED, q::JS_UNDEFINED, flags as c_int) < 0;
    });
    listed && !failed
}

pub(crate) unsafe fn JS_SetImmutablePrototype(cx: *mut root::JSContext, obj: root::JS::HandleObject, succeeded: *mut bool) -> bool {
    let cx = Cx::from_raw(cx);
    q::JS_SetImmutablePrototype(cx.qctx(), obj_val(*obj.ptr));
    *succeeded = true;
    true
}

pub(crate) unsafe fn GetObjectProto(cx: *mut root::JSContext, obj: root::JS::HandleObject, proto: root::JS::MutableHandleObject) -> bool {
    super::JS_GetPrototype(cx, obj, proto)
}

/// The prototype in the object's shape, without exotic `getPrototypeOf` hooks.
pub(crate) unsafe fn GetStaticPrototype(obj: *mut root::JSObject) -> *mut root::JSObject {
    let cx = super::current();
    let proto = q::JS_GetStaticPrototype(cx.base_ctx, obj_val(obj));
    let proto = cx.adopt(proto);
    if proto.is_object() { proto.to_object() } else { ptr::null_mut() }
}

pub(crate) unsafe fn JS_IsGlobalObject(obj: *mut root::JSObject) -> bool {
    !super::realm::realm_of_global(obj).is_null()
}

pub(crate) unsafe fn JS_LinkConstructorAndPrototype(cx: *mut root::JSContext, ctor: root::JS::Handle<*mut root::JSObject>, proto: root::JS::Handle<*mut root::JSObject>) -> bool {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let (ctor, proto) = (obj_val(*ctor.ptr), obj_val(*proto.ptr));
    let throw = q::JS_PROP_THROW as c_int;
    let ctor_proto_flags = throw;
    let proto_ctor_flags = (q::JS_PROP_CONFIGURABLE | q::JS_PROP_WRITABLE) as c_int | throw;
    ok(q::JS_DefinePropertyValueStr(qctx, ctor, c"prototype".as_ptr(), q::JS_DupValueRT(cx.rt, proto), ctor_proto_flags))
        && ok(q::JS_DefinePropertyValueStr(qctx, proto, c"constructor".as_ptr(), q::JS_DupValueRT(cx.rt, ctor), proto_ctor_flags))
}

pub(crate) unsafe fn JS_HasInstance(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, v: root::JS::Handle<root::JS::Value>, bp: *mut bool) -> bool {
    let cx = Cx::from_raw(cx);
    let r = q::JS_IsInstanceOf(cx.qctx(), (*v.ptr).to_qjs(), obj_val(*obj.ptr));
    *bp = r > 0;
    ok(r)
}

pub(crate) unsafe fn GetBuiltinClass(cx: *mut root::JSContext, obj: root::JS::Handle<*mut root::JSObject>, cls: *mut root::js::ESClass) -> bool {
    use root::js::ESClass;
    let cx = Cx::from_raw(cx);
    let obj = *obj.ptr;
    let class = class_of(obj);
    *cls = match class {
        QJS_CLASS_OBJECT => ESClass::Object,
        CLASS_ARRAY => ESClass::Array,
        CLASS_NUMBER => ESClass::Number,
        CLASS_STRING => ESClass::String,
        CLASS_BOOLEAN => ESClass::Boolean,
        CLASS_REGEXP => ESClass::RegExp,
        CLASS_ARRAY_BUFFER => ESClass::ArrayBuffer,
        CLASS_SHARED_ARRAY_BUFFER => ESClass::SharedArrayBuffer,
        CLASS_DATE => ESClass::Date,
        CLASS_SET => ESClass::Set,
        CLASS_MAP => ESClass::Map,
        CLASS_PROMISE => ESClass::Promise,
        CLASS_MAP_ITERATOR => ESClass::MapIterator,
        CLASS_SET_ITERATOR => ESClass::SetIterator,
        CLASS_ARGUMENTS | CLASS_MAPPED_ARGUMENTS => ESClass::Arguments,
        CLASS_ERROR => ESClass::Error,
        CLASS_BIG_INT => ESClass::BigInt,
        CLASS_PROXY => ESClass::Other,
        // Native functions and QuickJS functions are SpiderMonkey `JSFunction`s; callable objects
        // of SpiderMonkey classes are not.
        _ if object_data(obj).is_none() && q::JS_IsFunction(cx.qctx(), obj_val(obj)) != 0 => ESClass::Function,
        _ => ESClass::Other,
    };
    true
}

pub(crate) unsafe fn IsArgumentsObject(obj: root::JS::HandleObject) -> bool {
    matches!(class_of(*obj.ptr), CLASS_ARGUMENTS | CLASS_MAPPED_ARGUMENTS)
}

pub(crate) unsafe fn GetRealmObjectPrototype(cx: *mut root::JSContext) -> *mut root::JSObject {
    class_proto(cx, QJS_CLASS_OBJECT)
}

pub(crate) unsafe fn GetRealmFunctionPrototype(cx: *mut root::JSContext) -> *mut root::JSObject {
    class_proto(cx, QJS_CLASS_BYTECODE_FUNCTION)
}

pub(crate) unsafe fn GetRealmErrorPrototype(cx: *mut root::JSContext) -> *mut root::JSObject {
    class_proto(cx, CLASS_ERROR)
}

pub(crate) unsafe fn GetRealmIteratorPrototype(cx: *mut root::JSContext) -> *mut root::JSObject {
    class_proto(cx, CLASS_ITERATOR)
}

// Standard classes: QuickJS contexts define all of them eagerly, so global resolve and
// enumerate hooks have nothing to add.

pub(crate) unsafe fn JS_ResolveStandardClass(cx: *mut root::JSContext, obj: root::JS::HandleObject, id: root::JS::HandleId, resolved: *mut bool) -> bool {
    let _ = (cx, obj, id);
    *resolved = false;
    true
}

pub(crate) unsafe fn JS_MayResolveStandardClass(names: *const root::JSAtomState, id: root::jsid, maybeObj: *mut root::JSObject) -> bool {
    let _ = (names, id, maybeObj);
    false
}

pub(crate) unsafe fn JS_EnumerateStandardClasses(cx: *mut root::JSContext, obj: root::JS::HandleObject) -> bool {
    let _ = (cx, obj);
    true
}

pub(crate) unsafe fn JS_NewEnumerateStandardClasses(cx: *mut root::JSContext, obj: root::JS::HandleObject, properties: root::JS::MutableHandleIdVector, enumerableOnly: bool) -> bool {
    let _ = (cx, obj, properties, enumerableOnly);
    true
}
