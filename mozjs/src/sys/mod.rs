/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! Replacement for the `mozjs_sys` crate: SpiderMonkey's bindgen types, with every function
//! turned into a Rust stub that is progressively implemented on QuickJS.

mod jsimpls;

pub mod glue;
pub mod jsgc;
pub mod jsid;
pub mod jsval;
pub mod trace;

pub use self::generated::root as jsapi;

#[doc(hidden)]
#[allow(dead_code, unused_variables, clippy::all)]
mod generated {
    #![allow(unnecessary_transmutes)]
    include!(concat!(env!("OUT_DIR"), "/jsapi.rs"));
}

// The bindgen statics used to be `extern` and so needed no `Sync`; as Rust statics they do. They are
// only ever touched from the script thread.
unsafe impl<T> Sync for jsapi::JS::Handle<T> {}
unsafe impl Sync for jsapi::JSClassOps {}
unsafe impl Sync for jsapi::js::ClassExtension {}
unsafe impl Sync for jsapi::js::ObjectOps {}
unsafe impl Sync for jsapi::js::Wrapper {}
unsafe impl Sync for jsapi::js::CrossCompartmentWrapper {}
unsafe impl Sync for jsapi::js::OpaqueCrossCompartmentWrapper {}
