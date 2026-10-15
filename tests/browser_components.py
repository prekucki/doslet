#!/usr/bin/env python3
"""Real Firefox storage/WASM and local HTTPS Range/CORS acceptance; no Node.

Requires a Trunk build, Firefox and openssl. The generated TLS key is ephemeral
fixture data; Firefox's isolated profile accepts its self-signed certificate.
"""
import functools
import base64
import http.server
import json
import io
import os
from pathlib import Path
import re
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import zipfile
from browser_smoke import DIST, ROOT, Marionette


def iso_fixture():
    def record(sector, size, flags, name):
        entry = bytearray((34 + len(name)) & ~1)
        entry[0] = len(entry)
        entry[2:6], entry[6:10] = sector.to_bytes(4, 'little'), sector.to_bytes(4, 'big')
        entry[10:14], entry[14:18] = size.to_bytes(4, 'little'), size.to_bytes(4, 'big')
        entry[25], entry[28:32], entry[32] = flags, bytes([1, 0, 0, 1]), len(name)
        entry[33:33 + len(name)] = name
        return entry
    # Keep the file outside the first 128-KiB cache chunk so guest reads must
    # suspend and fetch another range after the volume header is available.
    sectors, file_sector = 1024, 700
    image = bytearray(sectors * 2048)
    pvd = 16 * 2048
    image[pvd:pvd + 7] = b'\x01CD001\x01'
    image[pvd + 80:pvd + 88] = sectors.to_bytes(4, 'little') + sectors.to_bytes(4, 'big')
    image[pvd + 128:pvd + 132] = bytes([0, 8, 8, 0])
    root = record(20, 2048, 2, b'\0')
    image[pvd + 156:pvd + 156 + len(root)] = root
    image[17 * 2048:17 * 2048 + 7] = b'\xffCD001\x01'
    offset = 20 * 2048
    for entry in [root, record(20, 2048, 2, b'\1'), record(file_sector, 4, 0, b'FILE.TXT;1')]:
        image[offset:offset + len(entry)] = entry
        offset += len(entry)
    image[file_sector * 2048:file_sector * 2048 + 4] = b'data'
    return bytes(image)


class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_GET(self):
        if self.path.startswith('/components.html'):
            bridge = next(DIST.glob('snippets/*/src/browser/bridge.js')).relative_to(DIST)
            data = (ROOT / 'tests/browser_components.html').read_text().replace('__BRIDGE__', '/' + str(bridge)).encode()
            self.send_response(200)
            self.send_header('Content-Type', 'text/html')
            self.end_headers()
            self.wfile.write(data)
        elif self.path in ['/save.zip', '/pad.zip', '/cd.zip']:
            program = bytearray(0x120)
            code = bytes([0xBA,0,2,0x31,0xC9,0xB4,0x3C,0xCD,0x21,0x89,0xC3,0xBA,0x10,2,0xB9,4,0,0xB4,0x40,0xCD,0x21,0xB4,0x3E,0xCD,0x21,0xB8,0,0x4C,0xCD,0x21])
            program[:len(code)] = code
            program[0x100:0x109] = b'SAVE.SAV\0'
            program[0x110:0x114] = b'save'
            name = 'SAVE.COM'
            if self.path == '/pad.zip':
                name = 'PAD.COM'
                # Wait for joystick button 1 (active-low bit 4), then exit 37.
                program = bytes([0xBA,1,2,0xEC,0xA8,0x10,0x75,0xFB,0xB8,37,0x4C,0xCD,0x21])
            elif self.path == '/cd.zip':
                name = 'CD.COM'
                program = bytearray(0x120)
                code = bytes([0xBA,0,2,0xB8,0,0x3D,0xCD,0x21,0x89,0xC3,0xBA,0,3,0xB9,4,0,0xB4,0x3F,0xCD,0x21,0xB4,0x3E,0xCD,0x21,0xA0,0,3,0xB4,0x4C,0xCD,0x21])
                program[:len(code)] = code
                path = b'D:\\FILE.TXT\0'
                program[0x100:0x100 + len(path)] = path
            output = io.BytesIO()
            with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
                archive.writestr(name, program)
            self.send_response(200)
            self.send_header('Content-Type', 'application/zip')
            self.end_headers()
            self.wfile.write(output.getvalue())
        else:
            super().do_GET()


class Media(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def cors(self):
        if self.path != '/no-cors':
            self.send_header('Access-Control-Allow-Origin', '*')
            self.send_header('Access-Control-Allow-Headers', 'If-Match, Range')
            self.send_header('Access-Control-Expose-Headers', 'Content-Range, ETag, Content-Encoding')
            self.send_header('Access-Control-Allow-Methods', 'GET, OPTIONS')

    def do_OPTIONS(self):
        self.send_response(204)
        self.cors()
        self.end_headers()

    def do_GET(self):
        image = iso_fixture() if self.path == '/volume' else None
        total = len(image) if image else 1024 * 1024
        if self.path == '/health':
            self.send_response(200)
            self.send_header('Content-Type', 'text/plain')
            self.send_header('Content-Length', '2')
            self.end_headers()
            self.wfile.write(b'OK')
            return
        match = re.fullmatch(r'bytes=(\d+)-(\d+)', self.headers.get('Range', ''))
        if not match:
            self.send_error(400)
            return
        start, end = map(int, match.groups())
        end = min(end, total - 1)
        if self.path == '/changed' and self.headers.get('If-Match'):
            self.send_response(412)
            self.cors()
            self.end_headers()
            return
        self.send_response(200 if self.path == '/ignored' else 206)
        self.cors()
        self.send_header('ETag', 'W/"fixture"' if self.path == '/weak' else '"fixture"')
        self.send_header('Content-Range', f'bytes {start + (self.path == "/wrong")}-{end}/{total}')
        self.send_header('Content-Length', str(end - start + 1))
        self.end_headers()
        try:
            self.wfile.write(image[start:end + 1] if image else bytes(index % 251 for index in range(start, end + 1)))
        except (BrokenPipeError, ConnectionResetError, ssl.SSLError):
            pass


def main():
    with tempfile.TemporaryDirectory(prefix='doslet-components-') as directory:
        profile = Path(directory)
        cert, key = profile / 'cert.pem', profile / 'key.pem'
        subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
                        '-subj', '/CN=localhost', '-addext', 'subjectAltName=DNS:localhost,IP:127.0.0.1',
                        '-keyout', str(key), '-out', str(cert)], check=True, capture_output=True)
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(cert, key)
        page = http.server.ThreadingHTTPServer(('127.0.0.1', 0), functools.partial(Handler, directory=str(DIST)))
        media = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Media)
        for server in [page, media]:
            server.socket = context.wrap_socket(server.socket, server_side=True)
            threading.Thread(target=server.serve_forever, daemon=True).start()
        with socket.socket() as reserve:
            reserve.bind(('127.0.0.1', 0))
            port = reserve.getsockname()[1]
        (profile / 'user.js').write_text(f'user_pref("marionette.port", {port});\nuser_pref("media.autoplay.default", 0);\n')
        with (ROOT / 'target/browser-components-firefox.log').open('w') as log:
            firefox = subprocess.Popen(['firefox', '--headless', '--no-remote', '--marionette', '--profile', str(profile)],
                                       stdout=log, stderr=log, env={**os.environ, 'MOZ_HEADLESS': '1'})
            try:
                deadline = time.monotonic() + 20
                while True:
                    try:
                        connection = socket.create_connection(('127.0.0.1', port), timeout=60)
                        break
                    except ConnectionRefusedError:
                        if time.monotonic() > deadline or firefox.poll() is not None:
                            raise RuntimeError('Firefox startup failed; see target/browser-components-firefox.log')
                        time.sleep(.1)
                driver = Marionette(connection)
                driver.call('WebDriver:NewSession', {'acceptInsecureCerts': True})
                origin = f'https://localhost:{media.server_port}'
                # Establish the fixture TLS connection before cross-origin Fetch.
                driver.call('WebDriver:Navigate', {'url': origin + '/health'})
                driver.call('WebDriver:Navigate', {'url': f'https://localhost:{page.server_port}/components.html?media={origin}'})
                driver.wait('window.result?.done || window.result?.error', timeout=90)
                result = driver.script('return window.result;')
                print(json.dumps(result, indent=2))
                assert result.get('done'), result
                driver.call('WebDriver:Navigate', {'url': f'https://localhost:{page.server_port}/'})
                driver.wait('document.querySelector("canvas")?.width > 0')
                time.sleep(1)
                screenshot = driver.call('WebDriver:TakeScreenshot', {'id': None, 'highlights': [], 'full': True})['value']
                (ROOT / 'target/configuration-workspace.png').write_bytes(base64.b64decode(screenshot))
                driver.call('WebDriver:PerformActions', {'actions': [{'type': 'pointer', 'id': 'mouse',
                    'parameters': {'pointerType': 'mouse'}, 'actions': [
                        {'type': 'pointerMove', 'duration': 0, 'origin': 'viewport', 'x': driver.script('return innerWidth - 114'), 'y': 26},
                        {'type': 'pointerDown', 'button': 0}, {'type': 'pointerUp', 'button': 0}]}]})
                time.sleep(.5)
                screenshot = driver.call('WebDriver:TakeScreenshot', {'id': None, 'highlights': [], 'full': True})['value']
                (ROOT / 'target/configuration-editor.png').write_bytes(base64.b64decode(screenshot))
            finally:
                firefox.terminate()
                firefox.wait(timeout=10)
                page.shutdown()
                media.shutdown()


if __name__ == '__main__':
    main()
