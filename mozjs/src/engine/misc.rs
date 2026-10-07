/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at http://mozilla.org/MPL/2.0/. */

//! GC, memory reporting and engine options; mostly no-ops on QuickJS.

#[allow(unused_imports)]
use quickjs_sys as q;

#[allow(unused_imports)]
use super::Cx;
#[allow(unused_imports)]
use crate::sys::jsapi as root;
