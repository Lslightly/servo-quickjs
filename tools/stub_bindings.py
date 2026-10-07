#!/usr/bin/env python3
"""Turn bindgen output for SpiderMonkey into a link-free Rust module.

Every `extern "C"` function becomes a Rust function whose body is `qjs_todo!(...)`, so that the
mozjs API type-checks without SpiderMonkey. Functions are later replaced by QuickJS
implementations, see `IMPLEMENTED`. Variadic functions stay `extern` declarations, statics get a
zeroed value.

Usage: stub_bindings.py <bindgen.rs> <out.rs> [<engine-src-dir>]
"""

import re
import sys


def skip_string(text, i):
    """Return the index just past the string literal starting at text[i] == '"'."""
    i += 1
    while text[i] != '"':
        i += 2 if text[i] == "\\" else 1
    return i + 1


def find_block_end(text, start):
    """Index of the '}' closing the '{' at text[start]."""
    depth = 0
    i = start
    while True:
        c = text[i]
        if c == '"':
            i = skip_string(text, i)
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1


def split_items(body):
    """Split an extern block body into items ending with ';' at nesting depth 0."""
    items, depth, cur, i = [], 0, [], 0
    while i < len(body):
        c = body[i]
        if c == '"':
            j = skip_string(body, i)
            cur.append(body[i:j])
            i = j
            continue
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        cur.append(c)
        if c == ";" and depth == 0:
            items.append("".join(cur))
            cur = []
        i += 1
    rest = "".join(cur).strip()
    assert not rest, rest[:200]
    return items


def param_names(params):
    names, depth, cur = [], 0, ""
    for c in params + ",":
        if c in "([<{":
            depth += 1
        elif c in ")]>}":
            depth -= 1
        if c == "," and depth == 0:
            if cur.strip():
                names.append(cur.split(":", 1)[0].strip())
            cur = ""
        else:
            cur += c
    return names


FN = re.compile(r"(?s)^(?P<attrs>.*?)(?P<vis>pub )fn (?P<name>\w+)\s*\((?P<params>.*)\)(?P<ret>\s*->\s*.*?)?;\s*$")
STATIC = re.compile(r"(?s)^(?P<attrs>.*?)pub static (?P<mut>mut )?(?P<name>\w+):\s*(?P<ty>.*?);\s*$")
LINK_NAME = re.compile(r'#\[link_name = "(?:[^"\\]|\\.)*"\]\s*')


def convert_item(item, indent, implemented, stats):
    text = item.strip()
    m = FN.match(text)
    if m:
        params = m.group("params")
        if re.search(r"(^|,)\s*\.\.\.\s*$", params):
            stats["variadic"] += 1
            return None
        attrs = LINK_NAME.sub("", m.group("attrs"))
        ret = m.group("ret") or ""
        name = m.group("name")
        stats["fn"] += 1
        if name in implemented:
            args = ", ".join(param_names(params))
            return f"{indent}{attrs.strip()}\n{indent}pub unsafe extern \"C\" fn {name}({params}){ret} {{ crate::engine::{name}({args}) }}\n"
        return f"{indent}{attrs.strip()}\n{indent}pub unsafe extern \"C\" fn {name}({params}){ret} {{ qjs_todo!(\"{name}\") }}\n"
    m = STATIC.match(text)
    if m:
        stats["static"] += 1
        attrs = LINK_NAME.sub("", m.group("attrs"))
        # Raw pointers are never `Sync`; everything else gets an `unsafe impl Sync` in `sys`.
        mut = "mut " if m.group("mut") or m.group("ty").lstrip().startswith("*") else ""
        return f"{indent}{attrs.strip()}\n{indent}pub static {mut}{m.group('name')}: {m.group('ty')} = unsafe {{ ::core::mem::zeroed() }};\n"
    raise ValueError("unrecognised extern item: " + text[:300])


def convert(text, implemented):
    stats = {"fn": 0, "static": 0, "variadic": 0}
    out = []
    pos = 0
    pat = re.compile(r'(?m)^(?P<indent>[ \t]*)(?:unsafe )?extern "C" \{')
    while True:
        m = pat.search(text, pos)
        if not m:
            out.append(text[pos:])
            break
        out.append(text[pos : m.start()])
        brace = m.end() - 1
        end = find_block_end(text, brace)
        body = text[brace + 1 : end]
        indent = m.group("indent")
        kept_extern = []
        for item in split_items(body):
            converted = convert_item(item, indent, implemented, stats)
            if converted is None:
                kept_extern.append(item)
            else:
                out.append(converted)
        if kept_extern:
            out.append(f'{indent}unsafe extern "C" {{' + "".join(kept_extern) + f"\n{indent}}}")
        pos = end + 1
    return "".join(out), stats


def main():
    src, dst = sys.argv[1], sys.argv[2]
    implemented = set()
    if len(sys.argv) > 3:
        # Functions implemented in the shim's engine module: `pub(crate) unsafe fn NAME(`.
        import glob
        for path in glob.glob(sys.argv[3] + "/**/*.rs", recursive=True):
            with open(path) as f:
                implemented |= set(re.findall(r"pub\(crate\) unsafe fn (\w+)\(", f.read()))
    with open(src) as f:
        text = f.read()
    # Layout assertions encode SpiderMonkey's sizes, which the shim changes (e.g. JS::Value).
    text = re.sub(
        r"#\[allow\(clippy::unnecessary_operation, clippy::identity_op\)\]\s*const _: \(\) = \{.*?\n\s*\};\n",
        "",
        text,
        flags=re.S,
    )
    # JS::Value uses QuickJS's 64-bit struct representation: payload bits plus a tag.
    text, n = re.subn(
        r"pub struct Value \{\s*pub asBits_: u64,\s*\}",
        "pub struct Value {\n            pub asBits_: u64,\n            pub tag: i64,\n        }",
        text,
    )
    assert n <= 1
    result, stats = convert(text, implemented)
    # mozjs_sys modules live under `crate::sys` in the shim.
    result = re.sub(r"\bcrate::(jsgc|jsval|jsid|glue|trace|jsapi)\b", r"crate::sys::\1", result)
    with open(dst, "w") as f:
        f.write(result)
    print(f"{dst}: {stats}", file=sys.stderr)


if __name__ == "__main__":
    main()
