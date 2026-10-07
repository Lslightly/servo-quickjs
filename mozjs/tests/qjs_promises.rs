/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Promises, the job queue, the rejection tracker, interrupts and CSP checks on QuickJS.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use mozjs::context::{JSContext, RawJSContext};
use mozjs::gc::StackGCVector;
use mozjs::glue::{CreateJobQueue, DeleteJobQueue, JobQueueTraps};
use mozjs::jsapi::{
    self, CallArgs, CompilationType, Handle, HandleObject, HandleString, HandleValue,
    HandleValueArray, Heap, JSObject, JSSecurityCallbacks, JSString, MutableHandleObject,
    MutableHandleValue, OnNewGlobalHookOption, PromiseRejectionHandlingState, PromiseState,
    RuntimeCode, Value,
};
use mozjs::jsval::{Int32Value, JSVal, ObjectValue, UndefinedValue};
use mozjs::realm::AutoRealm;
use mozjs::rooted;
use mozjs::rust::wrappers2::JS_NewGlobalObject;
use mozjs::rust::{
    evaluate_script, CompileOptionsWrapper, HandleObject as SafeHandleObject, JSEngine,
    RealmOptions, Runtime, ThreadSafeJSContext, SIMPLE_GLOBAL_CLASS,
};

/// Runs `f` in the realm of a new global of a new runtime.
fn with_global(
    engine: &JSEngine,
    f: impl FnOnce(ThreadSafeJSContext, &mut JSContext, SafeHandleObject),
) {
    let mut runtime = Runtime::new(engine.handle());
    let thread_safe = runtime.thread_safe_js_context();
    let context = runtime.cx();
    unsafe {
        rooted!(&in(context) let global = JS_NewGlobalObject(
            context,
            &SIMPLE_GLOBAL_CLASS,
            ptr::null_mut(),
            OnNewGlobalHookOption::FireOnNewGlobalHook,
            &*RealmOptions::default(),
        ));
        let mut realm = AutoRealm::new_from_handle(context, global.handle());
        let (global, realm) = realm.global_and_reborrow();
        f(thread_safe, realm, global);
    }
}

fn eval(cx: &mut JSContext, global: SafeHandleObject, source: &str) -> Result<JSVal, ()> {
    rooted!(&in(cx) let mut rval = UndefinedValue());
    let options = CompileOptionsWrapper::new(cx, c"test.js".to_owned(), 1);
    evaluate_script(cx, global, source, rval.handle_mut(), options)?;
    Ok(rval.get())
}

fn eval_bool(cx: &mut JSContext, global: SafeHandleObject, source: &str) -> bool {
    let v = eval(cx, global, source).unwrap_or_else(|_| panic!("{source} threw"));
    assert!(v.is_boolean(), "{source} is not a boolean");
    v.to_boolean()
}

unsafe fn handle<T>(ptr: &T) -> Handle<T> {
    Handle::from_marked_location(ptr)
}

unsafe fn new_promise(cx: &mut JSContext, executor: *mut JSObject) -> *mut JSObject {
    let executor = executor;
    let p = jsapi::NewPromiseObject(cx.raw_cx(), handle(&executor));
    assert!(!p.is_null());
    p
}

unsafe fn promise_state(p: *mut JSObject) -> PromiseState {
    jsapi::GetPromiseState(handle(&p))
}

unsafe fn promise_result(p: *mut JSObject) -> JSVal {
    let mut v = UndefinedValue();
    mozjs::glue::JS_GetPromiseResult(handle(&p), MutableHandleValue::from_marked_location(&mut v));
    v
}

/// Sets `global[name] = value`.
unsafe fn set_global(
    cx: &mut JSContext,
    global: SafeHandleObject,
    name: &std::ffi::CStr,
    value: JSVal,
) {
    let global = global.get();
    assert!(jsapi::JS_SetProperty(
        cx.raw_cx(),
        handle(&global),
        name.as_ptr(),
        handle(&value)
    ));
}

fn resolve_and_reject(cx: &mut JSContext, global: SafeHandleObject) {
    unsafe {
        let p = new_promise(cx, ptr::null_mut());
        assert!(jsapi::IsPromiseObject(handle(&p)));
        assert_eq!(promise_state(p), PromiseState::Pending);
        assert!(jsapi::ResolvePromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(42))
        ));
        assert_eq!(promise_state(p), PromiseState::Fulfilled);
        assert_eq!(promise_result(p).to_int32(), 42);
        // Settling again is a no-op.
        assert!(jsapi::RejectPromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(1))
        ));
        assert_eq!(promise_state(p), PromiseState::Fulfilled);

        let p = new_promise(cx, ptr::null_mut());
        assert!(jsapi::RejectPromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(7))
        ));
        assert_eq!(promise_state(p), PromiseState::Rejected);
        assert_eq!(promise_result(p).to_int32(), 7);

        // Resolving with a thenable adopts its state through jobs.
        let p = jsapi::CallOriginalPromiseResolve(cx.raw_cx(), handle(&Int32Value(5)));
        assert_eq!(promise_state(p), PromiseState::Fulfilled);
        assert_eq!(promise_result(p).to_int32(), 5);
        let same = jsapi::CallOriginalPromiseResolve(cx.raw_cx(), handle(&ObjectValue(p)));
        assert_eq!(same, p, "%Promise%.resolve(promise) is the promise itself");
        let p = jsapi::CallOriginalPromiseReject(cx.raw_cx(), handle(&Int32Value(6)));
        assert_eq!(promise_state(p), PromiseState::Rejected);

        let executor = eval(cx, global, "(function(resolve) { resolve(3); })").unwrap();
        let p = new_promise(cx, executor.to_object());
        assert_eq!(promise_state(p), PromiseState::Fulfilled);
        assert_eq!(promise_result(p).to_int32(), 3);
        let executor = eval(cx, global, "(function() { throw 4; })").unwrap();
        let p = new_promise(cx, executor.to_object());
        assert_eq!(promise_state(p), PromiseState::Rejected);
        assert_eq!(promise_result(p).to_int32(), 4);
        // Promises settled from Rust are visible to script.
        set_global(cx, global, c"p", ObjectValue(p));
        assert!(eval_bool(cx, global, "p instanceof Promise"));

        let plain = eval(cx, global, "({})").unwrap().to_object();
        assert!(!jsapi::IsPromiseObject(handle(&plain)));
        assert!(!jsapi::JS_IsExceptionPending(cx.raw_cx()));
    }
}

/// A job queue like Servo's microtask queue: jobs are function objects, run in order.
#[derive(Default)]
struct TestQueue {
    jobs: RefCell<VecDeque<Box<Heap<*mut JSObject>>>>,
    enqueued: Cell<usize>,
}

unsafe extern "C" fn get_host_defined_data(
    _: *const c_void,
    _: *mut RawJSContext,
    data: MutableHandleObject,
) -> bool {
    data.set(ptr::null_mut());
    true
}

unsafe extern "C" fn enqueue_promise_job(
    queue: *const c_void,
    _: *mut RawJSContext,
    _promise: HandleObject,
    job: HandleObject,
    _allocation_site: HandleObject,
    _host_defined_data: HandleObject,
) -> bool {
    let queue = &*(queue as *const TestQueue);
    queue.jobs.borrow_mut().push_back(Heap::boxed(*job.ptr));
    queue.enqueued.set(queue.enqueued.get() + 1);
    true
}

unsafe extern "C" fn run_jobs(queue: *const c_void, cx: *mut RawJSContext) {
    let queue = &*(queue as *const TestQueue);
    loop {
        let job = queue.jobs.borrow_mut().pop_front();
        let Some(job) = job else { break };
        let fun = ObjectValue(job.get());
        let mut rval = UndefinedValue();
        let args = HandleValueArray {
            length_: 0,
            elements_: ptr::null(),
        };
        assert!(jsapi::Call(
            cx,
            handle(&UndefinedValue()),
            handle(&fun),
            &args,
            MutableHandleValue::from_marked_location(&mut rval)
        ));
    }
}

unsafe extern "C" fn empty(queue: *const c_void) -> bool {
    (*(queue as *const TestQueue)).jobs.borrow().is_empty()
}

static INTERRUPT_QUEUES_DROPPED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn drop_interrupt_queues(_: *mut c_void) {
    INTERRUPT_QUEUES_DROPPED.store(true, Ordering::SeqCst);
}

static TRAPS: JobQueueTraps = JobQueueTraps {
    getHostDefinedData: Some(get_host_defined_data),
    enqueuePromiseJob: Some(enqueue_promise_job),
    runJobs: Some(run_jobs),
    empty: Some(empty),
    pushNewInterruptQueue: None,
    popInterruptQueue: None,
    dropInterruptQueues: Some(drop_interrupt_queues),
};

thread_local!(static FULFILLED: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) });

unsafe extern "C" fn on_fulfilled(_: *mut RawJSContext, argc: u32, vp: *mut Value) -> bool {
    let args = CallArgs::from_vp(vp, argc);
    FULFILLED.with(|f| f.borrow_mut().push(args.get(0).to_int32()));
    args.rval().set(UndefinedValue());
    true
}

fn job_queue(cx: &mut JSContext, global: SafeHandleObject) {
    let queue = TestQueue::default();
    unsafe {
        let job_queue = CreateJobQueue(
            &TRAPS,
            &queue as *const TestQueue as *const c_void,
            ptr::null_mut(),
        );
        jsapi::SetJobQueue(cx.raw_cx(), job_queue);

        assert!(eval_bool(
            cx,
            global,
            "
            var log = [];
            Promise.resolve(1).then(v => log.push(v)).then(() => log.push(3));
            Promise.resolve(2).then(v => log.push(v));
            (async () => { await null; log.push('a'); })();
            log.push('sync');
            log.join() === 'sync'
        "
        ));
        assert_eq!(queue.jobs.borrow().len(), 3);
        jsapi::RunJobs(cx.raw_cx());
        assert!(queue.jobs.borrow().is_empty());
        assert!(eval_bool(cx, global, "log.join() === 'sync,1,2,a,3'"));

        // Reactions added from Rust, settled from Rust.
        let p = new_promise(cx, ptr::null_mut());
        let f = jsapi::JS_GetFunctionObject(jsapi::JS_NewFunction(
            cx.raw_cx(),
            Some(on_fulfilled),
            1,
            0,
            c"onFulfilled".as_ptr(),
        ));
        let none: *mut JSObject = ptr::null_mut();
        assert!(jsapi::AddPromiseReactions(
            cx.raw_cx(),
            handle(&p),
            handle(&f),
            handle(&none)
        ));
        assert!(jsapi::GetPromiseIsHandled(handle(&p)));
        let before = queue.enqueued.get();
        assert!(jsapi::ResolvePromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(9))
        ));
        assert_eq!(queue.enqueued.get(), before + 1);
        assert!(FULFILLED.with(|f| f.borrow().is_empty()));
        jsapi::RunJobs(cx.raw_cx());
        assert_eq!(FULFILLED.with(|f| f.borrow().clone()), vec![9]);

        // Jobs that were never run are released with the queue.
        eval(cx, global, "Promise.resolve().then(() => {})").unwrap();
        assert_eq!(queue.jobs.borrow().len(), 1);
        queue.jobs.borrow_mut().clear();
        jsapi::SetJobQueue(cx.raw_cx(), ptr::null_mut());
        DeleteJobQueue(job_queue);
        assert!(INTERRUPT_QUEUES_DROPPED.load(Ordering::SeqCst));

        // Without a job queue, jobs use QuickJS's internal queue.
        assert!(eval_bool(
            cx,
            global,
            "var done = false; Promise.resolve().then(() => done = true); !done"
        ));
        jsapi::RunJobs(cx.raw_cx());
        assert!(eval_bool(cx, global, "done"));
    }
}

unsafe extern "C" fn track_rejection(
    _: *mut RawJSContext,
    muted_errors: bool,
    promise: HandleObject,
    state: PromiseRejectionHandlingState,
    data: *mut c_void,
) {
    assert!(!muted_errors);
    assert!(jsapi::IsPromiseObject(promise));
    let log = &*(data as *const RefCell<Vec<PromiseRejectionHandlingState>>);
    log.borrow_mut().push(state);
}

fn rejection_tracker(cx: &mut JSContext, global: SafeHandleObject) {
    let log: RefCell<Vec<PromiseRejectionHandlingState>> = Default::default();
    unsafe {
        jsapi::SetPromiseRejectionTrackerCallback(
            cx.raw_cx(),
            Some(track_rejection),
            &log as *const _ as *mut c_void,
        );
        let p = eval(cx, global, "var p = Promise.reject(1); p")
            .unwrap()
            .to_object();
        assert_eq!(*log.borrow(), [PromiseRejectionHandlingState::Unhandled]);
        assert!(!jsapi::GetPromiseIsHandled(handle(&p)));
        eval(cx, global, "p.catch(() => {})").unwrap();
        assert_eq!(
            *log.borrow(),
            [
                PromiseRejectionHandlingState::Unhandled,
                PromiseRejectionHandlingState::Handled
            ]
        );
        assert!(jsapi::GetPromiseIsHandled(handle(&p)));

        log.borrow_mut().clear();
        let p = new_promise(cx, ptr::null_mut());
        assert!(jsapi::RejectPromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(2))
        ));
        assert_eq!(*log.borrow(), [PromiseRejectionHandlingState::Unhandled]);
        assert!(jsapi::SetAnyPromiseIsHandled(cx.raw_cx(), handle(&p)));
        assert!(jsapi::GetPromiseIsHandled(handle(&p)));
        assert_eq!(
            *log.borrow(),
            [
                PromiseRejectionHandlingState::Unhandled,
                PromiseRejectionHandlingState::Handled
            ]
        );

        // Handled before rejection: no report.
        log.borrow_mut().clear();
        let p = new_promise(cx, ptr::null_mut());
        assert!(jsapi::SetAnyPromiseIsHandled(cx.raw_cx(), handle(&p)));
        assert!(jsapi::RejectPromise(
            cx.raw_cx(),
            handle(&p),
            handle(&Int32Value(3))
        ));
        assert!(log.borrow().is_empty());
        jsapi::SetPromiseRejectionTrackerCallback(cx.raw_cx(), None, ptr::null_mut());
    }
}

static INTERRUPTS: AtomicUsize = AtomicUsize::new(0);
static KEEP_RUNNING: AtomicBool = AtomicBool::new(true);

unsafe extern "C" fn interrupt_callback(_: *mut RawJSContext) -> bool {
    INTERRUPTS.fetch_add(1, Ordering::SeqCst);
    KEEP_RUNNING.load(Ordering::SeqCst)
}

fn interrupts(thread_safe: ThreadSafeJSContext, cx: &mut JSContext, global: SafeHandleObject) {
    unsafe {
        assert!(jsapi::JS_AddInterruptCallback(
            cx.raw_cx(),
            Some(interrupt_callback)
        ));

        // A callback that returns true lets the script continue.
        jsapi::JS_RequestInterruptCallback(cx.raw_cx());
        let v = eval(cx, global, "var i = 0; for (; i < 100000; i++) {} i").unwrap();
        assert_eq!(v.to_int32(), 100000);
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 1);

        // One that returns false terminates it, uncatchably.
        KEEP_RUNNING.store(false, Ordering::SeqCst);
        jsapi::JS_RequestInterruptCallback(cx.raw_cx());
        assert!(eval(
            cx,
            global,
            "var caught = false; try { for (;;) {} } catch (e) { caught = true; }"
        )
        .is_err());
        assert!(!jsapi::JS_IsExceptionPending(cx.raw_cx()));
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 2);
        assert!(!eval_bool(cx, global, "caught"));

        // Requests from another thread.
        let requester = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            thread_safe.request_interrupt_callback();
        });
        assert!(
            eval(cx, global, "for (let i = 0; i < 2e8; i++) {}").is_err(),
            "the loop was not interrupted"
        );
        requester.join().unwrap();
        assert!(!jsapi::JS_IsExceptionPending(cx.raw_cx()));
        assert_eq!(INTERRUPTS.load(Ordering::SeqCst), 3);
        KEEP_RUNNING.store(true, Ordering::SeqCst);
        assert_eq!(eval(cx, global, "1 + 1").unwrap().to_int32(), 2);
    }
}

thread_local!(static CSP_CHECKS: RefCell<Vec<(CompilationType, u32)>> = const { RefCell::new(Vec::new()) });

unsafe extern "C" fn content_security_policy_allows(
    _: *mut RawJSContext,
    runtime_code: RuntimeCode,
    _code_string: HandleString,
    compilation_type: CompilationType,
    parameter_strings: Handle<StackGCVector<*mut JSString>>,
    body_string: HandleString,
    parameter_args: Handle<StackGCVector<JSVal>>,
    _body_arg: HandleValue,
    can_compile_strings: *mut bool,
) -> bool {
    assert_eq!(runtime_code, RuntimeCode::JS);
    assert!(!body_string.is_null());
    let parameter_strings = mozjs::rust::Handle::from_raw(parameter_strings);
    let parameter_args = mozjs::rust::Handle::from_raw(parameter_args);
    assert_eq!(parameter_strings.len(), parameter_args.len());
    CSP_CHECKS.with(|c| {
        c.borrow_mut()
            .push((compilation_type, parameter_strings.len()))
    });
    *can_compile_strings = compilation_type == CompilationType::Function;
    true
}

static SECURITY_CALLBACKS: JSSecurityCallbacks = JSSecurityCallbacks {
    contentSecurityPolicyAllows: Some(content_security_policy_allows),
    codeForEvalGets: None,
    subsumes: None,
};

fn csp(cx: &mut JSContext, global: SafeHandleObject) {
    unsafe {
        jsapi::JS_SetSecurityCallbacks(cx.raw_cx(), &SECURITY_CALLBACKS);
        assert_eq!(
            jsapi::JS_GetSecurityCallbacks(cx.raw_cx()),
            &SECURITY_CALLBACKS as *const _
        );
        assert!(eval_bool(
            cx,
            global,
            "
            var errors = [];
            try { eval('1'); } catch (e) { errors.push(e instanceof EvalError); }
            try { (0, eval)('1'); } catch (e) { errors.push(e instanceof EvalError); }
            errors.join() === 'true,true'
        "
        ));
        assert_eq!(
            eval(cx, global, "Function('a', 'b', 'return a + b')(1, 2)")
                .unwrap()
                .to_int32(),
            3
        );
        // A non-string argument is returned by eval without a check.
        assert_eq!(eval(cx, global, "eval(5)").unwrap().to_int32(), 5);
        let checks = CSP_CHECKS.with(|c| c.borrow().clone());
        assert_eq!(
            checks,
            [
                (CompilationType::DirectEval, 0),
                (CompilationType::IndirectEval, 0),
                (CompilationType::Function, 2),
            ]
        );
        jsapi::JS_SetSecurityCallbacks(cx.raw_cx(), ptr::null());
        assert_eq!(eval(cx, global, "eval('6')").unwrap().to_int32(), 6);
    }
}

#[test]
fn promises() {
    let engine = JSEngine::init().unwrap();
    with_global(&engine, |_, cx, global| resolve_and_reject(cx, global));
    with_global(&engine, |_, cx, global| job_queue(cx, global));
    with_global(&engine, |_, cx, global| rejection_tracker(cx, global));
    with_global(&engine, interrupts);
    with_global(&engine, |_, cx, global| csp(cx, global));
}
