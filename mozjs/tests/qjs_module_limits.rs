/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Long module chains fail to load or link instead of overflowing the native stack, and module
//! private values hold a reference through the host's hooks, as with SpiderMonkey. The load hook
//! is asynchronous like Servo's network fetches: it queues each request, and `pump` finishes them
//! later.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_void, CString};
use std::marker::PhantomData;
use std::ptr;
use std::sync::OnceLock;

use mozjs::context::JSContext;
use mozjs::jsapi::{
    self, Heap, JSContext as RawJSContext, JSObject, JSScript, JSString, OnNewGlobalHookOption,
    Value,
};
use mozjs::jsval::{Int32Value, PrivateValue, UndefinedValue};
use mozjs::qjs;
use mozjs::rooted;
use mozjs::rust::wrappers2::{
    CompileModule1, CreateDefaultExportSyntheticModule, JS_NewGlobalObject, LoadRequestedModules1,
    ModuleLink, JS_GC,
};
use mozjs::rust::{
    transform_str_to_source_text, CompileOptionsWrapper, JSEngine, JSEngineHandle, RealmOptions,
    Runtime, SIMPLE_GLOBAL_CLASS,
};

const BASE: &str = "https://test/";

/// Modules in the chains; the native stack of the default test thread would overflow well before.
const CHAIN: usize = 20_000;

struct Pending {
    referrer: Box<Heap<*mut JSScript>>,
    request: Box<Heap<*mut JSObject>>,
    payload: Box<Heap<Value>>,
    url: String,
}

#[derive(Default)]
struct Loader {
    sources: HashMap<String, String>,
    modules: HashMap<String, Box<Heap<*mut JSObject>>>,
    pending: VecDeque<Pending>,
}

thread_local!(static LOADER: RefCell<Loader> = RefCell::new(Loader::default()));
thread_local!(static PRIVATE_REFS: RefCell<HashMap<usize, isize>> = RefCell::new(HashMap::new()));

/// Drops the module map before the runtime (its `Heap`s release references).
struct LoaderGuard;

impl Drop for LoaderGuard {
    fn drop(&mut self) {
        let loader = LOADER.with(|l| std::mem::take(&mut *l.borrow_mut()));
        drop(loader);
    }
}

fn h<T>(ptr: &T) -> jsapi::Handle<T> {
    jsapi::Handle {
        _phantom_0: PhantomData,
        ptr,
    }
}

fn url(specifier: &str) -> String {
    format!("{BASE}{}", specifier.trim_start_matches("./"))
}

unsafe fn string(s: *mut JSString) -> String {
    let v = qjs::JSValue::mkptr(qjs::JS_TAG_STRING, s as *mut c_void);
    let mut data = ptr::null();
    let mut wide = 0;
    let len = qjs::JS_GetStringData(v, &mut data, &mut wide);
    if wide != 0 {
        String::from_utf16(std::slice::from_raw_parts(data as *const u16, len)).unwrap()
    } else {
        std::slice::from_raw_parts(data as *const u8, len)
            .iter()
            .map(|&b| b as char)
            .collect()
    }
}

unsafe fn compile(cx: *mut RawJSContext, url: &str, source: &str) -> *mut JSObject {
    let mut context = JSContext::from_ptr(ptr::NonNull::new(cx).unwrap());
    let options = CompileOptionsWrapper::new(&mut context, CString::new(url).unwrap(), 1);
    let mut text = transform_str_to_source_text(source);
    let record = CompileModule1(&mut context, options.ptr, &mut text);
    if !record.is_null() {
        LOADER.with(|l| {
            l.borrow_mut()
                .modules
                .insert(url.to_owned(), Heap::boxed(record))
        });
    }
    record
}

/// Finishes one queued load: from the module map, else by "fetching" and compiling the source.
unsafe fn finish(cx: *mut RawJSContext, p: Pending) -> bool {
    let mut found = LOADER.with(|l| l.borrow().modules.get(&p.url).map(|m| m.get()));
    if found.is_none() {
        let source = LOADER.with(|l| l.borrow().sources.get(&p.url).cloned());
        let payload = p.payload.get();
        let Some(source) = source else {
            let error = UndefinedValue();
            return jsapi::FinishLoadingImportedModuleFailed(cx, h(&payload), h(&error));
        };
        let record = compile(cx, &p.url, &source);
        if record.is_null() {
            return jsapi::FinishLoadingImportedModuleFailedWithPendingException(cx, h(&payload));
        }
        found = Some(record);
    }
    let (referrer, request, payload) = (p.referrer.get(), p.request.get(), p.payload.get());
    let found = found.unwrap();
    jsapi::FinishLoadingImportedModule(cx, h(&referrer), h(&request), h(&payload), h(&found), true)
}

unsafe extern "C" fn load_hook(
    cx: *mut RawJSContext,
    referrer: jsapi::Handle<*mut JSScript>,
    request: jsapi::Handle<*mut JSObject>,
    _host_defined: jsapi::HandleValue,
    payload: jsapi::HandleValue,
    _line: u32,
    _column: jsapi::ColumnNumberOneOrigin,
) -> bool {
    let specifier = string(jsapi::GetModuleRequestSpecifier(cx, request));
    let pending = Pending {
        referrer: Heap::boxed(*referrer.ptr),
        request: Heap::boxed(*request.ptr),
        payload: Heap::boxed(*payload.ptr),
        url: url(&specifier),
    };
    LOADER.with(|l| l.borrow_mut().pending.push_back(pending));
    true
}

/// Finishes all queued loads, one per "task".
unsafe fn pump(cx: &mut JSContext) {
    while let Some(p) = LOADER.with(|l| l.borrow_mut().pending.pop_front()) {
        assert!(finish(cx.raw_cx(), p));
    }
}

unsafe fn load_requested(cx: &mut JSContext, root: *mut JSObject) -> *mut JSObject {
    rooted!(&in(cx) let root = root);
    rooted!(&in(cx) let host_defined = UndefinedValue());
    rooted!(&in(cx) let mut promise = ptr::null_mut::<JSObject>());
    assert!(LoadRequestedModules1(
        cx,
        root.handle(),
        host_defined.handle(),
        promise.handle_mut()
    ));
    promise.get()
}

fn promise_state(promise: *mut JSObject) -> qjs::JSPromiseStateEnum {
    unsafe {
        qjs::JS_PromiseState(
            ptr::null_mut(),
            qjs::JSValue::mkptr(qjs::JS_TAG_OBJECT, promise as *mut c_void),
        )
    }
}

fn mh<T>(ptr: &mut T) -> jsapi::MutableHandle<T> {
    jsapi::MutableHandle {
        _phantom_0: PhantomData,
        ptr,
    }
}

/// The `message` of an error object.
unsafe fn message(cx: &mut JSContext, error: Value) -> String {
    assert!(error.is_object());
    let obj = error.to_object();
    let mut message = UndefinedValue();
    assert!(jsapi::JS_GetProperty(
        cx.raw_cx(),
        h(&obj),
        c"message".as_ptr(),
        mh(&mut message)
    ));
    string(message.to_string())
}

/// A chain m0 -> m1 -> ... -> m(n-1) -> `last`.
fn add_chain(n: usize, last: &str) {
    LOADER.with(|l| {
        let mut l = l.borrow_mut();
        for i in 0..n {
            let next = if i + 1 == n {
                last.to_owned()
            } else {
                format!("m{}.js", i + 1)
            };
            l.sources.insert(
                url(&format!("m{i}.js")),
                format!("import './{next}';\nexport const v = {i};"),
            );
        }
    });
}

unsafe fn compile_root(cx: &mut JSContext) -> *mut JSObject {
    let source = LOADER.with(|l| l.borrow().sources[&url("m0.js")].clone());
    compile(cx.raw_cx(), &url("m0.js"), &source)
}

fn engine() -> JSEngineHandle {
    static ENGINE: OnceLock<JSEngineHandle> = OnceLock::new();
    ENGINE
        .get_or_init(|| {
            let engine = JSEngine::init().unwrap();
            let handle = engine.handle();
            std::mem::forget(engine);
            handle
        })
        .clone()
}

unsafe extern "C" fn add_ref_private(private: *const Value) {
    let id = (*private).to_private() as usize;
    PRIVATE_REFS.with(|r| *r.borrow_mut().entry(id).or_default() += 1);
}

unsafe extern "C" fn release_private(private: *const Value) {
    let id = (*private).to_private() as usize;
    PRIVATE_REFS.with(|r| *r.borrow_mut().entry(id).or_default() -= 1);
}

fn private_refs(id: usize) -> isize {
    PRIVATE_REFS.with(|r| r.borrow().get(&id).copied().unwrap_or(0))
}

fn with_realm(f: impl FnOnce(&mut JSContext)) {
    let mut runtime = Runtime::new(engine());
    let _loader = LoaderGuard;
    let cx = runtime.cx();
    unsafe {
        rooted!(&in(cx) let global = JS_NewGlobalObject(
            cx,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = mozjs::realm::AutoRealm::new_from_handle(cx, global.handle());
        let cx = &mut *realm;
        let rt = jsapi::JS_GetRuntime(cx.raw_cx());
        jsapi::SetModuleLoadHook(rt, Some(load_hook));
        jsapi::SetScriptPrivateReferenceHooks(rt, Some(add_ref_private), Some(release_private));
        f(cx);
    }
}

/// The first load fetches one module per task and fails at the end of the chain. Loading the
/// graph again walks the loaded chain recursively; it must fail instead of overflowing.
#[test]
fn long_chain_loaded_again() {
    with_realm(|cx| unsafe {
        add_chain(CHAIN, "missing.js");
        rooted!(&in(cx) let root = compile_root(cx));
        rooted!(&in(cx) let first = load_requested(cx, root.get()));
        pump(cx);
        assert_eq!(promise_state(first.get()), qjs::JS_PROMISE_REJECTED);
        rooted!(&in(cx) let second = load_requested(cx, root.get()));
        pump(cx);
        assert_eq!(promise_state(second.get()), qjs::JS_PROMISE_REJECTED);
        rooted!(&in(cx) let mut reason = UndefinedValue());
        mozjs::glue::JS_GetPromiseResult(second.handle().into(), reason.handle_mut().into());
        assert_eq!(message(cx, reason.get()), "stack overflow");
    });
}

/// A chain that loads; linking recurses over it in QuickJS and must fail instead of overflowing.
#[test]
fn long_chain_link() {
    with_realm(|cx| unsafe {
        add_chain(CHAIN, "end.js");
        LOADER.with(|l| {
            l.borrow_mut()
                .sources
                .insert(url("end.js"), "export const end = 1;".to_owned())
        });
        rooted!(&in(cx) let root = compile_root(cx));
        rooted!(&in(cx) let promise = load_requested(cx, root.get()));
        pump(cx);
        assert_eq!(promise_state(promise.get()), qjs::JS_PROMISE_FULFILLED);
        assert!(!ModuleLink(cx, root.handle()));
        let mut error = UndefinedValue();
        assert!(jsapi::JS_GetPendingException(cx.raw_cx(), mh(&mut error)));
        jsapi::JS_ClearPendingException(cx.raw_cx());
        assert_eq!(message(cx, error), "stack overflow");
    });
}

/// SetModulePrivate takes a reference through the add-ref hook and gives back the old value's
/// through the release hook, ClearModulePrivate gives it back, and so does freeing the module.
#[test]
fn module_private_holds_a_reference() {
    with_realm(|cx| unsafe {
        let options = CompileOptionsWrapper::new(cx, c"https://test/p.js".to_owned(), 1);
        let mut text = transform_str_to_source_text("export const x = 1;");
        rooted!(&in(cx) let module = CompileModule1(cx, options.ptr, &mut text));
        assert!(!module.get().is_null());
        jsapi::SetModulePrivate(module.get(), &PrivateValue(7 as *const c_void));
        assert_eq!(private_refs(7), 1);
        jsapi::SetModulePrivate(module.get(), &PrivateValue(8 as *const c_void));
        assert_eq!((private_refs(7), private_refs(8)), (0, 1));
        jsapi::ClearModulePrivate(module.get());
        assert_eq!(private_refs(8), 0);
        jsapi::SetModulePrivate(module.get(), &PrivateValue(9 as *const c_void));

        rooted!(&in(cx) let default = Int32Value(1));
        rooted!(&in(cx) let synthetic = CreateDefaultExportSyntheticModule(cx, default.handle()));
        assert!(!synthetic.get().is_null());
        jsapi::SetModulePrivate(synthetic.get(), &PrivateValue(10 as *const c_void));
        assert_eq!((private_refs(9), private_refs(10)), (1, 1));
    });
    // Freed with the runtime at the latest.
    assert_eq!((private_refs(9), private_refs(10)), (0, 0));
}

/// A module lives as long as its realm, and releases its private value when the collector frees
/// them.
#[test]
fn freed_module_releases_its_private() {
    with_realm(|cx| unsafe {
        {
            rooted!(&in(cx) let global = JS_NewGlobalObject(
                cx,
                &SIMPLE_GLOBAL_CLASS,
                ptr::null_mut(),
                OnNewGlobalHookOption::FireOnNewGlobalHook,
                &*RealmOptions::default(),
            ));
            let mut realm = mozjs::realm::AutoRealm::new_from_handle(cx, global.handle());
            let cx = &mut *realm;
            let options = CompileOptionsWrapper::new(cx, c"https://test/q.js".to_owned(), 1);
            let mut text = transform_str_to_source_text("export const y = 2;");
            rooted!(&in(cx) let module = CompileModule1(cx, options.ptr, &mut text));
            jsapi::SetModulePrivate(module.get(), &PrivateValue(11 as *const c_void));
        }
        assert_eq!(private_refs(11), 1);
        mozjs::quickjs::drain_autorelease_pool();
        JS_GC(cx, jsapi::GCReason::API);
        assert_eq!(private_refs(11), 0);
    });
}
