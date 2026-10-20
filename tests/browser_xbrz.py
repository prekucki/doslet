#!/usr/bin/env python3
"""Check the production iced xBRZ UI in Firefox using stdlib Marionette.

Build web/dist with Rust Trunk first. No Node or external Python packages.
"""
import argparse
import base64
import functools
import http.server
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time

from browser_smoke import Marionette, ROOT, DIST


HOOKS = """
window.xbrzTest = {errors: [], warnings: [], programs: 0, draws: 0};
window.addEventListener('error', e => xbrzTest.errors.push(e.message));
window.addEventListener('unhandledrejection', e => xbrzTest.errors.push(String(e.reason)));
const warn = console.warn;
console.warn = function(...args) { xbrzTest.warnings.push(args.map(String).join(' ')); warn.apply(this, args); };
// Exercise wgpu's WebGL path even where WebGPU is available.
Object.defineProperty(navigator, 'gpu', {value: undefined, configurable: true});
const shaders = new WeakSet(), programs = new WeakSet();
const gl = WebGL2RenderingContext.prototype;
const source = gl.shaderSource, attach = gl.attachShader;
const use = gl.useProgram;
gl.shaderSource = function(shader, code) {
  if (code.includes('blend_corner')) shaders.add(shader);
  return source.call(this, shader, code);
};
gl.attachShader = function(program, shader) {
  if (shaders.has(shader)) { programs.add(program); xbrzTest.programs++; }
  return attach.call(this, program, shader);
};
gl.useProgram = function(program) {
  this.xbrzActive = programs.has(program);
  return use.call(this, program);
};
for (const name of ['drawArrays', 'drawArraysInstanced', 'drawElements', 'drawElementsInstanced']) {
  const draw = gl[name];
  gl[name] = function(...args) {
    const result = draw.apply(this, args);
    if (this.xbrzActive) xbrzTest.draws++;
    return result;
  };
}
"""


def click(driver, x, y):
    driver.call("WebDriver:PerformActions", {"actions": [{
        "type": "pointer", "id": "mouse", "parameters": {"pointerType": "mouse"},
        "actions": [{"type": "pointerMove", "duration": 0, "x": x, "y": y},
                    {"type": "pointerDown", "button": 0},
                    {"type": "pointerUp", "button": 0}],
    }]})


def screenshot(driver, name):
    data = driver.call("WebDriver:TakeScreenshot", {})["value"]
    (ROOT / "target" / name).write_bytes(base64.b64decode(data))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--software", action="store_true", help="check Canvas2D fallback")
    args = parser.parse_args()
    mode = "software" if args.software else "webgl"
    page = (DIST / "index.html").read_text().replace("<head>", "<head><script>" + HOOKS + "</script>", 1).encode()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self):
            if self.path == "/":
                self.send_response(200)
                self.send_header("Content-Type", "text/html")
                self.end_headers()
                self.wfile.write(page)
            else:
                super().do_GET()

        def log_message(self, *_args):
            pass

    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), functools.partial(Handler, directory=str(DIST)))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="doslet-xbrz-firefox-") as directory:
        profile = Path(directory)
        with socket.socket() as port:
            port.bind(("127.0.0.1", 0))
            marionette_port = port.getsockname()[1]
        prefs = {"marionette.port": marionette_port, "media.volume_scale": "0.0",
                 "browser.shell.checkDefaultBrowser": False,
                 "webgl.disabled": args.software}
        (profile / "user.js").write_text("\n".join(f"user_pref({json.dumps(k)}, {json.dumps(v)});" for k, v in prefs.items()))
        with (ROOT / f"target/browser-xbrz-{mode}-firefox.log").open("w") as log:
            firefox = subprocess.Popen(["firefox", "--headless", "--no-remote", "--marionette", "--profile", str(profile)],
                                       stdout=log, stderr=log, env={**os.environ, "MOZ_HEADLESS": "1"})
            try:
                deadline = time.monotonic() + 25
                while True:
                    try:
                        connection = socket.create_connection(("127.0.0.1", marionette_port), timeout=60)
                        break
                    except ConnectionRefusedError:
                        if time.monotonic() > deadline or firefox.poll() is not None:
                            raise RuntimeError(f"Firefox startup failed; see target/browser-xbrz-{mode}-firefox.log")
                        time.sleep(0.1)
                driver = Marionette(connection)
                driver.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {}}})
                driver.call("WebDriver:SetWindowRect", {"width": 1120, "height": 860})
                driver.call("WebDriver:Navigate", {"url": f"http://127.0.0.1:{server.server_port}/"})
                driver.wait("document.querySelector('canvas') || xbrzTest.errors.length", timeout=90)
                time.sleep(3)
                startup = driver.script("const gl=document.querySelector('canvas')?.getContext('webgl2'); const ext=gl?.getExtension('WEBGL_debug_renderer_info'); return {errors:xbrzTest.errors,gpu:!!gl,renderer:gl?(ext?gl.getParameter(ext.UNMASKED_RENDERER_WEBGL):gl.getParameter(gl.RENDERER)):null};")
                assert not startup["errors"], startup
                assert startup["gpu"] != args.software, startup
                print("Renderer: " + json.dumps(startup), flush=True)
                click(driver, 980, 28)
                time.sleep(1)
                height = driver.script("return innerHeight;")
                click(driver, 790, height - 25)
                time.sleep(0.5)
                if args.software:
                    sample = driver.script("return Array.from(document.querySelector('canvas').getContext('2d').getImageData(200,180,8,8).data);")
                    assert any(sample[index] for index in range(len(sample)) if index % 4 != 3)
                click(driver, 560, height // 2 + 74)
                time.sleep(0.3)
                screenshot(driver, f"browser-xbrz-{mode}-options.png")
                # iced opens this menu upward; xBRZ is its final row.
                click(driver, 430, height // 2 + 39)
                time.sleep(1)
                screenshot(driver, f"browser-xbrz-{mode}-selected.png")
                if not args.software:
                    driver.wait("xbrzTest.draws > 0 || xbrzTest.errors.length", timeout=20)
                    state = driver.script("return {errors:xbrzTest.errors,programs:xbrzTest.programs,draws:xbrzTest.draws};")
                    assert not state["errors"], state
                    assert state["programs"] > 0 and state["draws"] > 0, state
                    print("xBRZ WebGL shader compiled and rendered: " + json.dumps(state), flush=True)
                else:
                    state = driver.script("return xbrzTest;")
                    assert not state["errors"] and state["draws"] == 0, state
                    assert any("xBRZ unavailable" in warning for warning in state["warnings"]), state
                    # The notice changes viewport height. Dismiss it before
                    # comparing the same on-screen sample at the original size.
                    click(driver, 772, height // 2 - 124)
                    click(driver, 1090, height - 66)
                    click(driver, 790, height - 25)
                    time.sleep(0.5)
                    after = driver.script("return Array.from(document.querySelector('canvas').getContext('2d').getImageData(200,180,8,8).data);")
                    assert after == sample, "Canvas2D fallback must retain the guest image"
                    print("Canvas2D: xBRZ rejected with notice, previous image preserved", flush=True)
                # Return to Nearest, then force a redraw by resizing the viewport.
                click(driver, 560, height // 2 + 74)
                click(driver, 430, height // 2 - 63)
                time.sleep(0.4)
                draws = driver.script("return xbrzTest.draws;")
                driver.call("WebDriver:SetWindowRect", {"width": 1000, "height": 800})
                time.sleep(0.5)
                assert driver.script("return xbrzTest.draws;") == draws, "Nearest must stop xBRZ draws"
                assert not driver.script("return xbrzTest.errors;"), driver.script("return xbrzTest.errors;")
                print("Switching back to Nearest and resizing: OK", flush=True)
            finally:
                firefox.terminate()
                try:
                    firefox.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    firefox.kill()
                    firefox.wait()
                server.shutdown()


if __name__ == "__main__":
    main()
