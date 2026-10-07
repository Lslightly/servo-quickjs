/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! ES modules: compile, link, evaluate and the host hooks of SpiderMonkey's module API.
//!
//! Servo loads module graphs itself: it compiles a module, lists its requests, fetches and
//! compiles the requested modules, and links once the graph is complete. QuickJS modules are
//! compiled without loading their requests (`JS_EVAL_FLAG_NO_RESOLVE`), and `JS_LinkModule`
//! resolves each request through Servo's resolve hook (patch Q7).
//!
//! A module record is an object of the `ModuleObject` class holding one reference to its
//! `JSModuleDef`; QuickJS's context also keeps every module alive until the realm dies. The
//! private value of a JavaScript module lives in the `JSModuleDef`, where the import.meta and
//! import() hooks find it. Module requests are objects of the `ModuleRequest` class.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr};
use std::marker::PhantomData;
use std::ptr;
use std::sync::OnceLock;

use quickjs_sys as q;

use super::eval::{options, padded, source_buffer, utf16_source};
use super::{adopt_object, obj_val, val_obj, Cx, RealmData};
use crate::sys::jsapi as root;
use crate::sys::jsval::JSVal;

#[derive(Clone, Copy, PartialEq)]
enum ModuleKind {
    JavaScript,
    /// A synthetic module whose default export is the parsed JSON (kept in the `JSModuleDef`'s
    /// private value until evaluation).
    Json,
}

/// Opaque data of `ModuleObject` objects.
struct ModuleData {
    /// One reference (a `JS_TAG_MODULE` value).
    module: *mut q::JSModuleDef,
    kind: ModuleKind,
    /// The `SetModulePrivate` value of JSON modules (counted).
    private: q::JSValue,
}

/// Opaque data of `ModuleRequest` objects.
struct RequestData {
    /// A counted string.
    specifier: q::JSValue,
    module_type: root::JS::ModuleType,
}

static CLASSES: OnceLock<(q::JSClassID, q::JSClassID)> = OnceLock::new();

/// The `ModuleObject` and `ModuleRequest` class ids, registered in `rt` on first use.
unsafe fn classes(rt: *mut q::JSRuntime) -> (q::JSClassID, q::JSClassID) {
    let ids = *CLASSES.get_or_init(|| {
        let (mut module, mut request) = (0, 0);
        q::JS_NewClassID(&mut module);
        q::JS_NewClassID(&mut request);
        (module, request)
    });
    if q::JS_IsRegisteredClass(rt, ids.0) == 0 {
        let module = q::JSClassDef {
            class_name: c"ModuleObject".as_ptr(),
            finalizer: Some(module_finalize),
            gc_mark: Some(module_gc_mark),
            call: None,
            exotic: ptr::null_mut(),
        };
        assert_eq!(q::JS_NewClass(rt, ids.0, &module), 0);
        let request = q::JSClassDef {
            class_name: c"ModuleRequest".as_ptr(),
            finalizer: Some(request_finalize),
            gc_mark: None,
            call: None,
            exotic: ptr::null_mut(),
        };
        assert_eq!(q::JS_NewClass(rt, ids.1, &request), 0);
    }
    ids
}

fn module_val(m: *mut q::JSModuleDef) -> q::JSValue {
    q::JSValue::mkptr(q::JS_TAG_MODULE, m as *mut c_void)
}

unsafe fn opaque_of<T>(v: q::JSValue, class: fn((q::JSClassID, q::JSClassID)) -> q::JSClassID) -> *mut T {
    match CLASSES.get() {
        Some(ids) => q::JS_GetOpaque(v, class(*ids)) as *mut T,
        None => ptr::null_mut(),
    }
}

unsafe fn module_data<'a>(obj: *mut root::JSObject) -> Option<&'a mut ModuleData> {
    opaque_of::<ModuleData>(obj_val(obj), |ids| ids.0).as_mut()
}

unsafe fn request_data<'a>(obj: *mut root::JSObject) -> Option<&'a mut RequestData> {
    opaque_of::<RequestData>(obj_val(obj), |ids| ids.1).as_mut()
}

unsafe fn module_of(obj: *mut root::JSObject) -> &'static mut ModuleData {
    module_data(obj).expect("not a module record")
}

unsafe extern "C" fn module_finalize(rt: *mut q::JSRuntime, val: q::JSValue) {
    let data = opaque_of::<ModuleData>(val, |ids| ids.0);
    if !data.is_null() {
        let data = Box::from_raw(data);
        q::JS_FreeValueRT(rt, module_val(data.module));
        q::JS_FreeValueRT(rt, data.private);
    }
}

unsafe extern "C" fn module_gc_mark(rt: *mut q::JSRuntime, val: q::JSValue, mark: q::JS_MarkFunc) {
    let data = opaque_of::<ModuleData>(val, |ids| ids.0);
    if let Some(data) = data.as_ref() {
        q::JS_MarkValue(rt, module_val(data.module), mark);
        q::JS_MarkValue(rt, data.private, mark);
    }
}

unsafe extern "C" fn request_finalize(rt: *mut q::JSRuntime, val: q::JSValue) {
    let data = opaque_of::<RequestData>(val, |ids| ids.1);
    if !data.is_null() {
        q::JS_FreeValueRT(rt, Box::from_raw(data).specifier);
    }
}

/// A module record for `module` (a reference the caller owns), retained by the pool.
unsafe fn new_module_object(cx: &mut Cx, module: q::JSValue, kind: ModuleKind) -> *mut root::JSObject {
    let (class, _) = classes(cx.rt);
    let obj = q::JS_NewObjectProtoClass(cx.qctx(), q::JS_NULL, class);
    if obj.tag() == q::JS_TAG_EXCEPTION {
        q::JS_FreeValueRT(cx.rt, module);
        return ptr::null_mut();
    }
    let data = Box::new(ModuleData {
        module: module.ptr() as *mut q::JSModuleDef,
        kind,
        private: q::JS_UNDEFINED,
    });
    q::JS_SetOpaque(obj, Box::into_raw(data) as *mut c_void);
    adopt_object(cx, obj)
}

/// A new module request object (a new reference) taking over `specifier`.
unsafe fn new_module_request(cx: &mut Cx, specifier: q::JSValue, module_type: root::JS::ModuleType) -> q::JSValue {
    let (_, class) = classes(cx.rt);
    let obj = q::JS_NewObjectProtoClass(cx.qctx(), q::JS_NULL, class);
    if obj.tag() == q::JS_TAG_EXCEPTION {
        q::JS_FreeValueRT(cx.rt, specifier);
        return obj;
    }
    let data = Box::new(RequestData { specifier, module_type });
    q::JS_SetOpaque(obj, Box::into_raw(data) as *mut c_void);
    obj
}

fn handle<T>(ptr: &T) -> root::JS::Handle<T> {
    root::JS::Handle { _phantom_0: PhantomData, ptr }
}

/// Runs `f` in the realm of the QuickJS context `ctx` (hooks called by QuickJS).
unsafe fn in_realm_of<R>(cx: &mut Cx, ctx: *mut q::JSContext, f: impl FnOnce(*mut root::JSContext) -> R) -> R {
    let realm = q::JS_GetContextOpaque(ctx) as *mut RealmData;
    if !realm.is_null() {
        cx.enter_realm(realm);
    }
    let result = f(cx.as_raw());
    if !realm.is_null() {
        cx.leave_realm();
    }
    result
}

/// A hook that fails without an exception is SpiderMonkey's uncatchable termination; QuickJS
/// needs an exception (see `call_native`).
unsafe fn ensure_exception(ctx: *mut q::JSContext) {
    if q::JS_HasException(ctx) == 0 {
        q::JS_ThrowInternalError(ctx, c"script terminated".as_ptr());
        q::JS_SetUncatchableException(ctx, 1);
    }
}

// Import attributes: HTML supports only `type`, and module types "json" and JavaScript.

/// Throws a SyntaxError (like SpiderMonkey) if `attributes` has a key other than `type`.
/// Whether all import attributes are supported (only `type` is); throws a SyntaxError for
/// static imports and a TypeError for import(), as SpiderMonkey does.
unsafe fn check_attributes(ctx: *mut q::JSContext, attributes: q::JSValue, dynamic: bool) -> bool {
    if attributes.tag() != q::JS_TAG_OBJECT {
        return true;
    }
    let mut tab: *mut q::JSPropertyEnum = ptr::null_mut();
    let mut len = 0;
    if q::JS_GetOwnPropertyNames(ctx, &mut tab, &mut len, attributes, q::JS_GPN_STRING_MASK as c_int) < 0 {
        return false;
    }
    let type_atom = q::JS_NewAtom(ctx, c"type".as_ptr());
    let mut ok = true;
    for i in 0..len as usize {
        let atom = (*tab.add(i)).atom;
        if atom != type_atom {
            let key = q::JS_AtomToCString(ctx, atom);
            if dynamic {
                q::JS_ThrowTypeError(ctx, c"Unsupported import attribute: %s".as_ptr(), key);
            } else {
                q::JS_ThrowSyntaxError(ctx, c"Unsupported import attribute: %s".as_ptr(), key);
            }
            q::JS_FreeCString(ctx, key);
            ok = false;
            break;
        }
    }
    q::JS_FreeAtom(ctx, type_atom);
    q::JS_FreePropertyEnum(ctx, tab, len);
    ok
}

unsafe fn module_type_of(ctx: *mut q::JSContext, attributes: q::JSValue) -> root::JS::ModuleType {
    if attributes.tag() != q::JS_TAG_OBJECT {
        return root::JS::ModuleType::JavaScript;
    }
    let value = q::JS_GetPropertyStr(ctx, attributes, c"type".as_ptr());
    let module_type = match value.tag() {
        q::JS_TAG_UNDEFINED => root::JS::ModuleType::JavaScript,
        q::JS_TAG_STRING => {
            let s = q::JS_ToCString(ctx, value);
            let json = !s.is_null() && CStr::from_ptr(s) == c"json";
            q::JS_FreeCString(ctx, s);
            if json { root::JS::ModuleType::JSON } else { root::JS::ModuleType::Unknown }
        },
        _ => root::JS::ModuleType::Unknown,
    };
    if value.tag() == q::JS_TAG_EXCEPTION {
        let exception = q::JS_GetException(ctx);
        q::JS_FreeValue(ctx, exception);
    }
    q::JS_FreeValue(ctx, value);
    module_type
}

/// The module request for request `idx` of `m` (a new reference), or `JS_EXCEPTION`.
unsafe fn requested_module(cx: &mut Cx, ctx: *mut q::JSContext, m: *mut q::JSModuleDef, idx: c_int) -> q::JSValue {
    let atom = q::JS_GetModuleRequestSpecifier(ctx, m, idx);
    if atom == q::JS_ATOM_NULL {
        return q::JS_ThrowRangeError(ctx, c"invalid module request index".as_ptr());
    }
    let specifier = q::JS_AtomToString(ctx, atom);
    q::JS_FreeAtom(ctx, atom);
    if specifier.tag() == q::JS_TAG_EXCEPTION {
        return specifier;
    }
    let attributes = q::JS_GetModuleRequestAttributes(ctx, m, idx);
    let module_type = module_type_of(ctx, attributes);
    q::JS_FreeValue(ctx, attributes);
    new_module_request(cx, specifier, module_type)
}

// Compilation.

unsafe fn compile_module(cx: &mut Cx, filename: &CStr, src: &[u8]) -> *mut root::JSObject {
    let flags = q::JS_EVAL_TYPE_MODULE | q::JS_EVAL_FLAG_COMPILE_ONLY | q::JS_EVAL_FLAG_NO_RESOLVE;
    let module = q::JS_Eval(cx.qctx(), src.as_ptr() as *const c_char, src.len() - 1, filename.as_ptr(), flags as c_int);
    if module.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    new_module_object(cx, module, ModuleKind::JavaScript)
}

unsafe extern "C" fn json_module_init(ctx: *mut q::JSContext, m: *mut q::JSModuleDef) -> c_int {
    let value = q::JS_GetModulePrivateValue(ctx, m);
    q::JS_SetModulePrivateValue(ctx, m, q::JS_UNDEFINED);
    q::JS_SetModuleExport(ctx, m, c"default".as_ptr(), value)
}

/// A synthetic module whose default export is the JSON value of `src` (NUL-terminated).
unsafe fn compile_json_module(cx: &mut Cx, filename: &CStr, src: &[u8]) -> *mut root::JSObject {
    let qctx = cx.qctx();
    let value = q::JS_ParseJSON(qctx, src.as_ptr() as *const c_char, src.len() - 1, filename.as_ptr());
    if value.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    let m = q::JS_NewCModule(qctx, filename.as_ptr(), Some(json_module_init));
    if m.is_null() || q::JS_AddModuleExport(qctx, m, c"default".as_ptr()) < 0 {
        q::JS_FreeValue(qctx, value);
        return ptr::null_mut();
    }
    q::JS_SetModulePrivateValue(qctx, m, value);
    let module = q::JS_DupValue(qctx, module_val(m));
    new_module_object(cx, module, ModuleKind::Json)
}

pub(crate) unsafe fn CompileModule1(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<root::mozilla::Utf8Unit>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = source_buffer(options.base.lineno, (*srcBuf).units_ as *const u8, (*srcBuf).length_ as usize);
    compile_module(cx, &options.filename, &src)
}

pub(crate) unsafe fn CompileModule(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<u16>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = padded(options.base.lineno, &utf16_source(srcBuf));
    compile_module(cx, &options.filename, &src)
}

pub(crate) unsafe fn CompileJsonModule1(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<root::mozilla::Utf8Unit>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = source_buffer(1, (*srcBuf).units_ as *const u8, (*srcBuf).length_ as usize);
    compile_json_module(cx, &options.filename, &src)
}

pub(crate) unsafe fn CompileJsonModule(cx: *mut root::JSContext, options: *const root::JS::ReadOnlyCompileOptions, srcBuf: *mut root::JS::SourceText<u16>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let options = self::options(options);
    let src = padded(1, &utf16_source(srcBuf));
    compile_json_module(cx, &options.filename, &src)
}

// Module records.

pub(crate) unsafe fn IsCyclicModule(module: *mut root::JSObject) -> bool {
    module_of(module).kind == ModuleKind::JavaScript
}

pub(crate) unsafe fn SetModulePrivate(module: *mut root::JSObject, value: *const root::JS::Value) {
    let cx = super::current();
    let data = module_of(module);
    let value = q::JS_DupValueRT(cx.rt, (*value).to_qjs());
    match data.kind {
        ModuleKind::JavaScript => {
            q::JS_SetModulePrivateValue(cx.base_ctx, data.module, value);
        },
        ModuleKind::Json => {
            let old = std::mem::replace(&mut data.private, value);
            q::JS_FreeValueRT(cx.rt, old);
        },
    }
}

pub(crate) unsafe fn JS_GetModulePrivate(module: *mut root::JSObject, dest: root::JS::MutableHandleValue) {
    let cx = super::current();
    let data = module_of(module);
    let value = match data.kind {
        ModuleKind::JavaScript => q::JS_GetModulePrivateValue(cx.base_ctx, data.module),
        ModuleKind::Json => q::JS_DupValueRT(cx.rt, data.private),
    };
    *dest.ptr = cx.adopt(value);
}

pub(crate) unsafe fn GetRequestedModulesCount(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>) -> u32 {
    let _ = cx;
    q::JS_GetModuleRequestCount(module_of(*moduleRecord.ptr).module) as u32
}

pub(crate) unsafe fn GetRequestedModuleSpecifier(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>, index: u32) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let m = module_of(*moduleRecord.ptr).module;
    let attributes = q::JS_GetModuleRequestAttributes(qctx, m, index as c_int);
    let supported = check_attributes(qctx, attributes, false);
    let module_type = module_type_of(qctx, attributes);
    q::JS_FreeValue(qctx, attributes);
    if !supported {
        return ptr::null_mut();
    }
    if module_type == root::JS::ModuleType::Unknown {
        q::JS_ThrowTypeError(qctx, c"invalid module type".as_ptr());
        return ptr::null_mut();
    }
    let atom = q::JS_GetModuleRequestSpecifier(qctx, m, index as c_int);
    if atom == q::JS_ATOM_NULL {
        q::JS_ThrowRangeError(qctx, c"invalid module request index".as_ptr());
        return ptr::null_mut();
    }
    let specifier = q::JS_AtomToString(qctx, atom);
    q::JS_FreeAtom(qctx, atom);
    if specifier.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    cx.adopt(specifier).to_string()
}

pub(crate) unsafe fn GetRequestedModuleType(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>, index: u32) -> root::JS::ModuleType {
    let qctx = Cx::from_raw(cx).qctx();
    let attributes = q::JS_GetModuleRequestAttributes(qctx, module_of(*moduleRecord.ptr).module, index as c_int);
    let module_type = module_type_of(qctx, attributes);
    q::JS_FreeValue(qctx, attributes);
    module_type
}

// Module requests.

pub(crate) unsafe fn CreateModuleRequest(cx: *mut root::JSContext, specifierArg: root::JS::Handle<*mut root::JSString>, moduleType: root::JS::ModuleType) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let specifier = q::JS_DupValueRT(cx.rt, q::JSValue::mkptr(q::JS_TAG_STRING, *specifierArg.ptr as *mut c_void));
    let request = new_module_request(cx, specifier, moduleType);
    adopt_object(cx, request)
}

pub(crate) unsafe fn GetModuleRequestSpecifier(cx: *mut root::JSContext, moduleRequestArg: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSString {
    let cx = Cx::from_raw(cx);
    let data = request_data(*moduleRequestArg.ptr).expect("not a module request");
    cx.retain(JSVal::from_qjs(data.specifier)).to_string()
}

pub(crate) unsafe fn GetModuleRequestType(cx: *mut root::JSContext, moduleRequestArg: root::JS::Handle<*mut root::JSObject>) -> root::JS::ModuleType {
    let _ = cx;
    request_data(*moduleRequestArg.ptr).expect("not a module request").module_type
}

// Link and evaluate.

/// `JSModuleResolveFunc` for `JS_LinkModule`: Servo's HostResolveImportedModule.
unsafe extern "C" fn resolve_imported_module(ctx: *mut q::JSContext, m: *mut q::JSModuleDef, idx: c_int, opaque: *mut c_void) -> *mut q::JSModuleDef {
    let _ = opaque;
    let cx = super::current();
    let Some(hook) = hooks(cx.rt).resolve else {
        q::JS_ThrowTypeError(ctx, c"no module resolve hook".as_ptr());
        return ptr::null_mut();
    };
    let request = requested_module(cx, ctx, m, idx);
    if request.tag() == q::JS_TAG_EXCEPTION {
        return ptr::null_mut();
    }
    let request = cx.adopt(request).to_object();
    let private = cx.adopt(module_private(ctx, m));
    let result = in_realm_of(cx, ctx, |raw| hook(raw, handle(&private), handle(&request)));
    if result.is_null() {
        ensure_exception(ctx);
        return ptr::null_mut();
    }
    match module_data(result) {
        Some(data) => data.module,
        None => {
            q::JS_ThrowTypeError(ctx, c"module resolve hook returned a non-module".as_ptr());
            ptr::null_mut()
        },
    }
}

pub(crate) unsafe fn ModuleLink(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>) -> bool {
    let cx = Cx::from_raw(cx);
    let m = module_of(*moduleRecord.ptr).module;
    q::JS_LinkModule(cx.qctx(), m, Some(resolve_imported_module), ptr::null_mut()) >= 0
}

pub(crate) unsafe fn ModuleEvaluate(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>, rval: root::JS::MutableHandleValue) -> bool {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let m = module_of(*moduleRecord.ptr).module;
    if q::JS_IsModuleLinked(m) == 0 {
        q::JS_ThrowTypeError(qctx, c"module is not linked".as_ptr());
        return false;
    }
    // Returns the evaluation promise (rejected on evaluation errors); consumes a reference.
    let promise = q::JS_EvalFunction(qctx, q::JS_DupValueRT(cx.rt, module_val(m)));
    if promise.tag() == q::JS_TAG_EXCEPTION {
        return false;
    }
    *rval.ptr = cx.adopt(promise);
    true
}

pub(crate) unsafe fn ThrowOnModuleEvaluationFailure(cx: *mut root::JSContext, evaluationPromise: root::JS::Handle<*mut root::JSObject>, errorBehaviour: root::JS::ModuleErrorBehaviour) -> bool {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let promise = *evaluationPromise.ptr;
    if promise.is_null() {
        return false;
    }
    if errorBehaviour == root::JS::ModuleErrorBehaviour::ReportModuleErrorsAsync {
        // A rejection is reported by the rejection tracker.
        return true;
    }
    let promise = obj_val(promise);
    // SpiderMonkey reports a pending (top-level await) promise as a failure with an undefined
    // exception; a pending evaluation has not failed (yet), so it is a success here.
    if q::JS_PromiseState(qctx, promise) != q::JS_PROMISE_REJECTED {
        return true;
    }
    q::JS_MarkPromiseHandled(qctx, promise);
    q::JS_Throw(qctx, q::JS_PromiseResult(qctx, promise));
    false
}

pub(crate) unsafe fn GetModuleNamespace(cx: *mut root::JSContext, moduleRecord: root::JS::Handle<*mut root::JSObject>) -> *mut root::JSObject {
    let cx = Cx::from_raw(cx);
    let qctx = cx.qctx();
    let m = module_of(*moduleRecord.ptr).module;
    if q::JS_IsModuleLinked(m) == 0 {
        q::JS_ThrowTypeError(qctx, c"module is not linked".as_ptr());
        return ptr::null_mut();
    }
    let namespace = q::JS_GetModuleNamespace(qctx, m);
    adopt_object(cx, namespace)
}

// Host hooks, per runtime.

#[derive(Clone, Copy, Default)]
struct Hooks {
    resolve: root::JS::ModuleResolveHook,
    metadata: root::JS::ModuleMetadataHook,
    dynamic_import: root::JS::ModuleDynamicImportHook,
    add_ref: root::JS::ScriptPrivateReferenceHook,
    release: root::JS::ScriptPrivateReferenceHook,
}

thread_local! {
    static HOOKS: RefCell<HashMap<usize, Hooks>> = RefCell::new(HashMap::new());
}

fn hooks(rt: *mut q::JSRuntime) -> Hooks {
    HOOKS.with(|h| h.borrow().get(&(rt as usize)).copied().unwrap_or_default())
}

fn update_hooks(rt: *mut root::JSRuntime, f: impl FnOnce(&mut Hooks)) {
    HOOKS.with(|h| f(h.borrow_mut().entry(rt as usize).or_default()));
}

pub(crate) unsafe fn SetModuleResolveHook(rt: *mut root::JSRuntime, func: root::JS::ModuleResolveHook) {
    update_hooks(rt, |h| h.resolve = func);
}

pub(crate) unsafe fn GetModuleResolveHook(rt: *mut root::JSRuntime) -> root::JS::ModuleResolveHook {
    hooks(rt as *mut q::JSRuntime).resolve
}

pub(crate) unsafe fn SetModuleMetadataHook(rt: *mut root::JSRuntime, func: root::JS::ModuleMetadataHook) {
    update_hooks(rt, |h| h.metadata = func);
    q::JS_SetImportMetaHook(rt as *mut q::JSRuntime, func.map(|_| import_meta_hook as _), ptr::null_mut());
}

pub(crate) unsafe fn GetModuleMetadataHook(rt: *mut root::JSRuntime) -> root::JS::ModuleMetadataHook {
    hooks(rt as *mut q::JSRuntime).metadata
}

pub(crate) unsafe fn SetModuleDynamicImportHook(rt: *mut root::JSRuntime, func: root::JS::ModuleDynamicImportHook) {
    update_hooks(rt, |h| h.dynamic_import = func);
    q::JS_SetDynamicImportHook(rt as *mut q::JSRuntime, func.map(|_| dynamic_import_hook as _), ptr::null_mut());
}

pub(crate) unsafe fn GetModuleDynamicImportHook(rt: *mut root::JSRuntime) -> root::JS::ModuleDynamicImportHook {
    hooks(rt as *mut q::JSRuntime).dynamic_import
}

pub(crate) unsafe fn SetScriptPrivateReferenceHooks(rt: *mut root::JSRuntime, addRefHook: root::JS::ScriptPrivateReferenceHook, releaseHook: root::JS::ScriptPrivateReferenceHook) {
    update_hooks(rt, |h| {
        h.add_ref = addRefHook;
        h.release = releaseHook;
    });
    q::JS_SetScriptPrivateFinalizer(rt as *mut q::JSRuntime, Some(release_script_private));
}

/// Takes a reference for a script private value stored by QuickJS, like SpiderMonkey's
/// `ScriptSourceObject::setPrivate`.
pub(crate) unsafe fn add_ref_script_private(rt: *mut q::JSRuntime, value: q::JSValue) {
    if let Some(add_ref) = hooks(rt).add_ref {
        add_ref(&JSVal::from_qjs(value));
    }
}

/// `JSScriptPrivateFinalizer`: releases the reference taken by `add_ref_script_private`.
unsafe extern "C" fn release_script_private(rt: *mut q::JSRuntime, value: q::JSValue) {
    let release = HOOKS.try_with(|h| h.borrow().get(&(rt as usize)).and_then(|h| h.release));
    if let Ok(Some(release)) = release {
        release(&JSVal::from_qjs(value));
    }
}

/// `JSImportMetaHook`: Servo's HostPopulateImportMeta, called when import.meta is first used.
unsafe extern "C" fn import_meta_hook(ctx: *mut q::JSContext, m: *mut q::JSModuleDef, meta: q::JSValue, opaque: *mut c_void) -> c_int {
    let _ = opaque;
    let cx = super::current();
    let Some(hook) = hooks(cx.rt).metadata else {
        return 0;
    };
    let private = cx.adopt(module_private(ctx, m));
    let meta = val_obj(meta);
    if in_realm_of(cx, ctx, |raw| hook(raw, handle(&private), handle(&meta))) {
        0
    } else {
        ensure_exception(ctx);
        -1
    }
}

/// The private value of a module (a new reference); C modules (JSON) have none.
unsafe fn module_private(ctx: *mut q::JSContext, m: *mut q::JSModuleDef) -> q::JSValue {
    if q::JS_IsCModule(m) != 0 {
        q::JS_UNDEFINED
    } else {
        q::JS_GetModulePrivateValue(ctx, m)
    }
}

/// The private value of a frame's module `m`, or else of its script (`script_private`,
/// borrowed). Retained by the pool.
pub(crate) unsafe fn script_or_module_private(cx: &mut Cx, ctx: *mut q::JSContext, m: *mut q::JSModuleDef, script_private: q::JSValue) -> JSVal {
    if m.is_null() {
        return cx.retain(JSVal::from_qjs(script_private));
    }
    let ctx = if ctx.is_null() { cx.qctx() } else { ctx };
    cx.adopt(module_private(ctx, m))
}

/// `JSDynamicImportHook`: Servo's HostImportModuleDynamically, which settles `promise`.
unsafe extern "C" fn dynamic_import_hook(
    ctx: *mut q::JSContext,
    m: *mut q::JSModuleDef,
    script_private: q::JSValue,
    specifier: q::JSValue,
    attributes: q::JSValue,
    promise: q::JSValue,
    opaque: *mut c_void,
) -> c_int {
    let _ = opaque;
    let cx = super::current();
    let hooks = hooks(cx.rt);
    let Some(hook) = hooks.dynamic_import else {
        q::JS_ThrowTypeError(ctx, c"no dynamic import hook".as_ptr());
        return -1;
    };
    if !check_attributes(ctx, attributes, true) {
        return -1;
    }
    let module_type = module_type_of(ctx, attributes);
    let request = new_module_request(cx, q::JS_DupValue(ctx, specifier), module_type);
    if request.tag() == q::JS_TAG_EXCEPTION {
        return -1;
    }
    let request = cx.adopt(request).to_object();
    let private = script_or_module_private(cx, ctx, m, script_private);
    let promise = val_obj(promise);
    let counted = !private.is_undefined();
    if counted {
        if let Some(add_ref) = hooks.add_ref {
            add_ref(&private);
        }
    }
    let ok = in_realm_of(cx, ctx, |raw| hook(raw, handle(&private), handle(&request), handle(&promise)));
    if counted {
        if let Some(release) = hooks.release {
            release(&private);
        }
    }
    if ok {
        0
    } else {
        ensure_exception(ctx);
        -1
    }
}
