// xBRZ freescale, adapted to WGSL for Doslet.
// https://github.com/libretro/slang-shaders/blob/master/edge-smoothing/xbrz/shaders/xbrz-freescale.slang
// Corner rules and blend order follow the reference; rotations share one kernel.
//
// Hyllian's xBR-vertex code and texel mapping
// Copyright (C) 2011/2016 Hyllian - sergiogdb@gmail.com
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in
// all copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
// THE SOFTWARE.
//
// This shader also uses code and/or concepts from xBRZ as it appears
// in the Desmume source code. The license for which is as follows:
// ****************************************************************************
// * This file is part of the HqMAME project. It is distributed under         *
// * GNU General Public License: http://www.gnu.org/licenses/gpl-3.0          *
// * Copyright (C) Zenju (zenju AT gmx DOT de) - All Rights Reserved          *
// *                                                                          *
// * Additionally and as a special exception, the author gives permission     *
// * to link the code of this program with the MAME library (or with modified *
// * versions of MAME that use the same license as MAME), and distribute      *
// * linked combinations including the two. You must obey the GNU General     *
// * Public License in all respects for all of the code used other than MAME. *
// * If you modify this file, you may extend this exception to your version   *
// * of the file, but you are not obligated to do so. If you do not wish to   *
// * do so, delete this exception statement from your version.                *
// ****************************************************************************

@group(0) @binding(0) var source: texture_2d<f32>;
override OUTPUT_SRGB: bool = true;

struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) uv: vec2f,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> Vertex {
    let uv = array<vec2f, 3>(vec2f(0.0, 0.0), vec2f(2.0, 0.0), vec2f(0.0, 2.0))[index];
    return Vertex(vec4f(uv * vec2f(2.0, -2.0) + vec2f(-1.0, 1.0), 0.0, 1.0), uv);
}

fn rotate(p: vec2i, turn: u32) -> vec2i {
    switch turn {
        case 1u: { return vec2i(-p.y, p.x); }
        case 2u: { return -p; }
        case 3u: { return vec2i(p.y, -p.x); }
        default: { return p; }
    }
}

fn tap(center: vec2i, offset: vec2i, turn: u32) -> vec3f {
    let limit = vec2i(textureDimensions(source)) - vec2i(1);
    return textureLoad(source, clamp(center + rotate(offset, turn), vec2i(0), limit), 0).rgb;
}

fn distance(a: vec3f, b: vec3f) -> f32 {
    let diff = a - b;
    let y = dot(diff, vec3f(0.2627, 0.6780, 0.0593));
    let cb = (0.5 / (1.0 - 0.0593)) * (diff.b - y);
    let cr = (0.5 / (1.0 - 0.2627)) * (diff.r - y);
    return length(vec3f(y, cb, cr));
}

fn equal(a: vec3f, b: vec3f) -> bool {
    return distance(a, b) < 30.0 / 255.0;
}

// Classify the bottom-right corner in each rotated 5x5 neighbourhood.
// 0 = no blend, 1 = normal gradient, 2 = dominant gradient.
fn corner(p: vec2i, turn: u32) -> u32 {
    let b = tap(p, vec2i(0, -1), turn);
    let c = tap(p, vec2i(1, -1), turn);
    let d = tap(p, vec2i(-1, 0), turn);
    let e = tap(p, vec2i(0, 0), turn);
    let f = tap(p, vec2i(1, 0), turn);
    let g = tap(p, vec2i(-1, 1), turn);
    let h = tap(p, vec2i(0, 1), turn);
    let i = tap(p, vec2i(1, 1), turn);
    if ((all(e == f) && all(h == i)) || (all(e == h) && all(f == i))) {
        return 0u;
    }
    let hf = distance(g, e) + distance(e, c)
        + distance(tap(p, vec2i(0, 2), turn), i)
        + distance(i, tap(p, vec2i(2, 0), turn)) + 4.0 * distance(h, f);
    let ei = distance(d, h) + distance(h, tap(p, vec2i(1, 2), turn))
        + distance(b, f) + distance(f, tap(p, vec2i(2, 1), turn)) + 4.0 * distance(e, i);
    if (hf < ei && any(e != f) && any(e != h)) {
        return select(1u, 2u, 3.6 * hf < ei);
    }
    return 0u;
}

fn left_ratio(pos: vec2f, origin: vec2f, direction: vec2f, scale: vec2f) -> f32 {
    let delta = pos - origin;
    let projection = direction * (dot(delta, direction) / dot(direction, direction));
    let side = sign(dot(delta, vec2f(-direction.y, direction.x)));
    let v = side * length((delta - projection) * scale);
    return smoothstep(-0.70710678118, 0.70710678118, v);
}

fn blend_corner(p: vec2i, turn: u32, corners: vec4u, pos: vec2f, scale: vec2f, previous: vec3f) -> vec3f {
    if (corners[turn] == 0u) { return previous; }
    let b = tap(p, vec2i(0, -1), turn);
    let c = tap(p, vec2i(1, -1), turn);
    let d = tap(p, vec2i(-1, 0), turn);
    let e = tap(p, vec2i(0, 0), turn);
    let f = tap(p, vec2i(1, 0), turn);
    let g = tap(p, vec2i(-1, 1), turn);
    let h = tap(p, vec2i(0, 1), turn);
    let i = tap(p, vec2i(1, 1), turn);
    let fg = distance(f, g);
    let hc = distance(h, c);
    let line = corners[turn] == 2u || !(
        (corners[(turn + 3u) % 4u] != 0u && !equal(e, g)) ||
        (corners[(turn + 1u) % 4u] != 0u && !equal(e, c)) ||
        (equal(g, h) && equal(h, i) && equal(i, f) && equal(f, c) && !equal(e, i)));
    var origin = vec2f(0.0, 0.70710678118);
    var direction = vec2f(1.0, -1.0);
    if (line) {
        let shallow = 2.2 * fg <= hc && any(e != g) && any(d != g);
        let steep = 2.2 * hc <= fg && any(e != c) && any(b != c);
        origin = vec2f(0.0, select(0.5, 0.25, shallow));
        direction += vec2f(select(0.0, 1.0, shallow), select(0.0, -1.0, steep));
    }
    // Bring the fragment back into the canonical corner's coordinate system.
    var local = pos;
    var local_scale = scale;
    switch turn {
        case 1u: { local = vec2f(pos.y, -pos.x); local_scale = scale.yx; }
        case 2u: { local = -pos; }
        case 3u: { local = vec2f(-pos.y, pos.x); local_scale = scale.yx; }
        default: {}
    }
    // Preserve the reference's tie-breaking and sequential blend order.
    let ef = distance(e, f);
    let eh = distance(e, h);
    let choose_f = select(ef < eh, ef <= eh, turn == 0u || turn == 3u);
    let pixel = select(h, f, choose_f);
    return mix(previous, pixel, left_ratio(local, origin, direction, local_scale));
}

fn linear_rgb(rgb: vec3f) -> vec3f {
    return select(pow((rgb + vec3f(0.055)) / 1.055, vec3f(2.4)), rgb / 12.92, rgb <= vec3f(0.04045));
}

@fragment
fn fs_main(input: Vertex) -> @location(0) vec4f {
    let location = input.uv * vec2f(textureDimensions(source));
    // Derivatives are evaluated before any divergent corner decisions. They
    // measure the actual physical viewport, including fractional sizes/HiDPI.
    let scale = vec2f(1.0) / max(fwidth(location), vec2f(0.000001));
    let p = vec2i(floor(location));
    let pos = fract(location) - vec2f(0.5);
    var result = tap(p, vec2i(0), 0u);
    // Avoid smoothing at 1:1 or when shrinking either axis.
    if (all(scale > vec2f(1.0001))) {
        let corners = vec4u(corner(p, 0u), corner(p, 1u), corner(p, 2u), corner(p, 3u));
        result = blend_corner(p, 0u, corners, pos, scale, result);
        result = blend_corner(p, 1u, corners, pos, scale, result);
        result = blend_corner(p, 3u, corners, pos, scale, result);
        result = blend_corner(p, 2u, corners, pos, scale, result);
    }
    // The input and blend metric use encoded RGB. Only sRGB targets require
    // linear output; the attachment then performs the encoding exactly once.
    if (OUTPUT_SRGB) { result = linear_rgb(result); }
    return vec4f(result, 1.0);
}
