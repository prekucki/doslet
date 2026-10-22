#!/usr/bin/env python3
"""Translate an mpv RAVU-Zoom user shader (bjin/mpv-prescalers) to WGSL.

Usage:
    python3 tools/ravu_to_wgsl.py ravu-zoom-ar-r3-rgb.hook apps/doslet/src/ravu \
        --source https://github.com/bjin/mpv-prescalers/blob/<commit>/ravu-zoom-ar-r3-rgb.hook

Writes `ravu.wgsl` and one little-endian f16 file per `//!TEXTURE` (named after
the texture) into the output directory. The .hook files are generated code
with a fixed shape: one `hook()` pass over 6×6 taps plus RGBA weight LUTs given
as float32 hex. The translation is mechanical; any construct it does not know
raises instead of being guessed. Standard library only.
"""
import argparse
import re
import struct
from pathlib import Path

TYPES = {"float": "f32", "vec2": "vec2f", "vec3": "vec3f", "vec4": "vec4f", "mat4x3": "mat4x3f"}
COMPARISON = re.compile(r"(<=|>=|<|>|==|!=)")


def split_args(text):
    """Split a call's argument text on top-level commas."""
    args, depth, start = [], 0, 0
    for i, c in enumerate(text):
        if c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "," and depth == 0:
            args.append(text[start:i].strip())
            start = i + 1
    args.append(text[start:].strip())
    return args


def rewrite_calls(expr, name, convert):
    """Replace every `name(...)` call, innermost arguments first."""
    out, i = [], 0
    pattern = re.compile(r"\b%s\(" % re.escape(name))
    while True:
        m = pattern.search(expr, i)
        if not m:
            out.append(expr[i:])
            return "".join(out)
        out.append(expr[i : m.start()])
        depth, j = 1, m.end()
        while depth:
            depth += {"(": 1, ")": -1}.get(expr[j], 0)
            j += 1
        args = [rewrite_calls(a, name, convert) for a in split_args(expr[m.end() : j - 1])]
        out.append(convert(args))
        i = j


def mix(args):
    # GLSL mix(x, y, bool) selects y when true; WGSL select(f, t, cond) too.
    if len(args) == 3 and COMPARISON.search(args[2]):
        return "select(%s, %s, %s)" % tuple(args)
    return "mix(%s)" % ", ".join(args)


def expression(expr):
    expr = re.sub(r"\bHOOKED_tex\(\(pos \+ vec2\(([^)]*)\)\) \* HOOKED_pt\)\.xyz", r"tap(pos + vec2f(\1))", expr)
    if "HOOKED" in expr:
        raise ValueError("unknown HOOKED use: " + expr)
    expr = rewrite_calls(expr, "mix", mix)
    expr = rewrite_calls(expr, "mod", lambda a: "glsl_mod(%s)" % ", ".join(a))
    expr = rewrite_calls(expr, "atan", lambda a: ("atan2(%s)" if len(a) == 2 else "atan(%s)") % ", ".join(a))
    expr = rewrite_calls(expr, "LUTPOS", lambda a: "lutpos(%s)" % ", ".join(a))
    expr = rewrite_calls(expr, "matrixCompMult", lambda a: "comp_mult(%s)" % ", ".join(a))
    expr = re.sub(
        r"\btexture\((\w+), ",
        lambda m: "textureSampleLevel(%s, lut_sampler, " % m.group(1),
        expr,
    )
    # textureSampleLevel needs the explicit level; add it to each converted call.
    expr = rewrite_calls(expr, "textureSampleLevel", lambda a: "textureSampleLevel(%s, 0.0)" % ", ".join(a))
    for glsl, wgsl in TYPES.items():
        expr = re.sub(r"\b%s\(" % glsl, wgsl + "(", expr)
    return expr


def statement(stmt):
    stmt = stmt.strip()
    if not stmt or stmt == "vec2 pos = HOOKED_pos * HOOKED_size":
        return []  # `pos` is declared by the template from the fragment uv.
    m = re.match(r"(float|vec2|vec3|vec4|mat4x3)\s+(.*)$", stmt)
    if m:
        lines = []
        for part in split_args(m.group(2)):
            name, _, value = part.partition("=")
            name = name.strip()
            if not re.fullmatch(r"\w+", name):
                raise ValueError("unknown declaration: " + stmt)
            if value:
                lines.append("var %s: %s = %s;" % (name, TYPES[m.group(1)], expression(value.strip())))
            else:
                lines.append("var %s: %s;" % (name, TYPES[m.group(1)]))
        return lines
    m = re.match(r"return vec4\((\w+), 1\.0\)$", stmt)
    if m:
        return ["return %s;" % m.group(1)]
    if re.match(r"\w+(\[\d\])?\s*(=|\+=|-=|\*=|/=)", stmt):
        return [expression(stmt) + ";"]
    raise ValueError("unknown statement: " + stmt)


def translate(hook, source):
    header = hook[: hook.index("//!DESC")]
    desc = re.search(r"//!DESC (.*)", hook).group(1)
    pre, body = hook.split("vec4 hook() {", 1)
    body = body[: body.index("\n}\n")]
    constants = []
    for line in pre.splitlines():
        m = re.match(r"const vec3 (\w+) = vec3\(([^)]*)\);", line)
        if m:
            constants.append("const %s = vec3f(%s);" % m.groups())
        elif line.startswith("#define LUTPOS"):
            pass  # Provided as `lutpos` below.
        elif line and not line.startswith("//"):
            raise ValueError("unknown preamble: " + line)
    textures = re.findall(r"//!TEXTURE (\w+)\n//!SIZE (\d+) (\d+)\n//!FORMAT (\w+)\n//!FILTER (\w+)\n([0-9a-f]+)", hook)
    code = []
    for stmt in body.replace("\n", "").split(";"):
        code.extend(statement(stmt))
    if not code or not code[-1].startswith("return "):
        raise ValueError("hook() must end with a return")
    result = code.pop()[len("return ") : -1]
    bindings = "\n".join(
        "@group(0) @binding(%d) var %s: texture_2d<f32>;" % (i + 1, name)
        for i, (name, *_rest) in enumerate(textures)
    )
    shader = HEADER.format(
        license=header.rstrip(),
        desc=desc,
        source=source,
        bindings=bindings,
        sampler_binding=len(textures) + 1,
        constants="\n".join(constants),
        body="\n".join("    " + line for line in code),
        result=result,
    )
    return shader, textures


HEADER = """{license}
//
// SPDX-License-Identifier: LGPL-3.0-or-later
// Copyright (C) Bin Jin, mpv-prescalers (https://github.com/bjin/mpv-prescalers)
// {desc}: generated by tools/ravu_to_wgsl.py from
// {source}
// Do not edit; regenerate instead. Doslet additions: source-edge clamping,
// sRGB target handling, the 16:9 smart stretch (WARP) and plain sampling when
// not enlarging, like the upstream WHEN condition.

@group(0) @binding(0) var source: texture_2d<f32>;
{bindings}
@group(0) @binding({sampler_binding}) var lut_sampler: sampler;
override OUTPUT_SRGB: bool = true;
override WARP: bool = false;

{constants}

struct Vertex {{
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Vertex {{
    let uv = array<vec2f, 3>(vec2f(0.0, 0.0), vec2f(2.0, 0.0), vec2f(0.0, 2.0))[index];
    return Vertex(vec4f(uv * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0), uv);
}}

// Texel at a pixel-centre position, clamped like CLAMP_TO_EDGE.
fn tap(p: vec2f) -> vec3f {{
    let limit = vec2i(textureDimensions(source)) - vec2i(1);
    return textureLoad(source, clamp(vec2i(floor(p)), vec2i(0), limit), 0).rgb;
}}

fn lutpos(x: vec2f, lut_size: vec2f) -> vec2f {{
    return mix(0.5 / lut_size, 1.0 - 0.5 / lut_size, x);
}}

fn glsl_mod(x: f32, y: f32) -> f32 {{
    return x - y * floor(x / y);
}}

fn comp_mult(a: mat4x3f, b: mat4x3f) -> mat4x3f {{
    return mat4x3f(a[0] * b[0], a[1] * b[1], a[2] * b[2], a[3] * b[3]);
}}

// Inverse of x = 0.5 + 0.75 d + d^3; see `stretch_source` in presentation.rs.
fn warp_x(x: f32) -> f32 {{
    let t = clamp(x, 0.0, 1.0) - 0.5;
    let m = abs(t);
    var d = m;
    for (var i = 0; i < 8; i++) {{
        d -= (d * (0.75 + d * d) - m) / (0.75 + 3.0 * d * d);
    }}
    return 0.5 + sign(t) * d;
}}

fn linear_rgb(rgb: vec3f) -> vec3f {{
    return select(pow((rgb + vec3f(0.055)) / 1.055, vec3f(2.4)), rgb / 12.92, rgb <= vec3f(0.04045));
}}

fn ravu(uv: vec2f) -> vec3f {{
    let HOOKED_size = vec2f(textureDimensions(source));
    var pos: vec2f = uv * HOOKED_size;
{body}
    return {result};
}}

@fragment
fn fs_main(input: Vertex) -> @location(0) vec4f {{
    var uv = input.uv;
    if (WARP) {{ uv.x = warp_x(uv.x); }}
    let location = uv * vec2f(textureDimensions(source));
    let scale = vec2f(1.0) / max(fwidth(location), vec2f(0.000001));
    var result: vec3f;
    // RAVU-Zoom only enlarges; at 1:1 or when shrinking show the texel.
    if (all(scale > vec2f(1.0001))) {{
        result = ravu(uv);
    }} else {{
        result = tap(location);
    }}
    if (OUTPUT_SRGB) {{ result = linear_rgb(clamp(result, vec3f(0.0), vec3f(1.0))); }}
    return vec4f(result, 1.0);
}}
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("hook", type=Path)
    parser.add_argument("out", type=Path)
    parser.add_argument("--source", required=True, help="upstream URL pinned to a commit")
    args = parser.parse_args()
    shader, textures = translate(args.hook.read_text(), args.source)
    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "ravu.wgsl").write_text(shader)
    for name, width, height, fmt, filt, data in textures:
        if fmt != "rgba16f" or filt != "LINEAR":
            raise ValueError("unexpected texture %s %s %s" % (name, fmt, filt))
        raw = bytes.fromhex(data)
        count = int(width) * int(height) * 4
        if len(raw) != count * 4:
            raise ValueError("%s: expected float32 data" % name)
        values = struct.unpack("<%df" % count, raw)
        (args.out / (name + ".f16")).write_bytes(struct.pack("<%de" % count, *values))
        print("%s %sx%s" % (name, width, height))


if __name__ == "__main__":
    main()
