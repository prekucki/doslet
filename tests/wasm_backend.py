#!/usr/bin/env python3
"""Build and test the real browser WASM translator with Firefox; no Node.

Uses the existing stdlib Marionette client. Artifacts stay under target/.
"""
import functools
import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time
from browser_smoke import Marionette, ROOT


def main():
    stage = ROOT / 'target/wasm-backend-browser'
    stage.mkdir(parents=True, exist_ok=True)
    subprocess.run(['cargo', 'build', '-p', 'doslet-wasm-probe', '--target', 'wasm32-unknown-unknown',
                    '--profile', 'web', '--locked', '--offline'], cwd=ROOT, check=True)
    subprocess.run(['wasm-bindgen', str(ROOT / 'target/wasm32-unknown-unknown/web/doslet_wasm_probe.wasm'),
                    '--target', 'web', '--no-typescript', '--out-dir', str(stage / 'pkg')], cwd=ROOT, check=True)
    shutil.copyfile(ROOT / 'tests/wasm_backend.html', stage / 'index.html')
    shutil.copyfile(ROOT / 'web/browser/wasm-core.js', stage / 'wasm-core.js')
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(stage))
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        with tempfile.TemporaryDirectory(prefix='doslet-wasm-backend-') as directory:
            profile = Path(directory)
            with socket.socket() as reserve:
                reserve.bind(('127.0.0.1', 0)); port = reserve.getsockname()[1]
            (profile / 'user.js').write_text(f'user_pref("marionette.port", {port});\n')
            with (ROOT / 'target/wasm-backend-firefox.log').open('w') as log:
                firefox = subprocess.Popen(['firefox', '--headless', '--no-remote', '--marionette', '--profile', str(profile)],
                                           stdout=log, stderr=log, env={**os.environ, 'MOZ_HEADLESS': '1'})
                connection = None
                try:
                    deadline = time.monotonic() + 20
                    while connection is None:
                        try:
                            connection = socket.create_connection(('127.0.0.1', port), timeout=30)
                        except ConnectionRefusedError:
                            if time.monotonic() > deadline or firefox.poll() is not None:
                                raise RuntimeError('Firefox startup failed; see target/wasm-backend-firefox.log')
                            time.sleep(.1)
                    driver = Marionette(connection)
                    driver.call('WebDriver:NewSession')
                    driver.call('WebDriver:Navigate', {'url': f'http://127.0.0.1:{server.server_port}/'})
                    driver.wait('window.result?.done', timeout=45)
                    result = driver.script('return window.result;')
                    (ROOT / 'target/wasm-backend-browser/result.json').write_text(json.dumps(result, indent=2) + '\n')
                    print(json.dumps(result, indent=2))
                    assert result.get('ok'), result
                finally:
                    if connection is not None: connection.close()
                    firefox.terminate()
                    try: firefox.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        firefox.kill(); firefox.wait(timeout=10)
    finally:
        server.shutdown(); server.server_close()


if __name__ == '__main__':
    main()
