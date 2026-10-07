#!/usr/bin/env python3
"""Print single-line SpiderMonkey signatures for the given function names, ready to paste into
`mozjs/src/engine` as `pub(crate) unsafe fn` (engine modules alias the jsapi module as `root`)."""
import os
import re
import sys

base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "mozjs", "upstream-bindgen")
text = "".join(open(os.path.join(base, f)).read() for f in ["jsapi.rs", "gluebindings.rs"])
for name in sys.argv[1:]:
    m = re.search(r"pub fn " + name + r"\((.*?)\)(\s*->\s*[^;]*?)?;", text, re.S)
    if not m:
        print(f"// {name}: not found")
        continue
    params = " ".join(m.group(1).split()).rstrip(",")
    if params.endswith("..."):
        print(f"// {name}: variadic")
        continue
    ret = " ".join((m.group(2) or "").split())
    print(f"pub(crate) unsafe fn {name}({params}){(' ' + ret) if ret else ''} {{")
