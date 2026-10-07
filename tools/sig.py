#!/usr/bin/env python3
"""Print single-line SpiderMonkey signatures (as stubbed in the shim) for the given function names,
ready to paste into `mozjs/src/engine` as `pub(crate) unsafe fn` (engine modules alias the jsapi
module as `root`)."""
import re
import sys

import os
base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mozjs", "src", "sys", "generated")
text = "".join(open(os.path.join(base, f)).read() for f in ["jsapi.rs", "gluebindings.rs"])
for name in sys.argv[1:]:
    m = re.search(r'pub unsafe extern "C" fn ' + name + r'\((.*?)\)(\s*->\s*[^{]*?)?\s*\{', text, re.S)
    if not m:
        print(f"// {name}: not found (variadic or missing)")
        continue
    params = " ".join(m.group(1).split()).rstrip(",")
    ret = " ".join((m.group(2) or "").split())
    print(f"pub(crate) unsafe fn {name}({params}){(' ' + ret) if ret else ''} {{")
