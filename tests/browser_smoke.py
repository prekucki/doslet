#!/usr/bin/env python3
"""Real Firefox Worker/AudioWorklet smoke test using stdlib Marionette; no Node.

Build web/dist first. An isolated headless profile allows autoplay and mutes the
audio sink; assertions establish rendered samples, not independent listening.
"""
import functools
import argparse
import http.server
import io
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import threading
import time
import zipfile

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "web/dist"


class Marionette:
    def __init__(self, connection):
        self.connection = connection
        self.counter = 0
        self.receive()

    def receive(self):
        length = bytearray()
        while True:
            byte = self.connection.recv(1)
            if not byte:
                raise RuntimeError("Marionette connection closed")
            if byte == b":":
                break
            length.extend(byte)
        data = bytearray()
        while len(data) < int(length):
            data.extend(self.connection.recv(int(length) - len(data)))
        return json.loads(data)

    def call(self, command, args=None):
        self.counter += 1
        data = json.dumps([0, self.counter, command, args or {}]).encode()
        self.connection.sendall(str(len(data)).encode() + b":" + data)
        result = self.receive()
        if result[2]:
            raise RuntimeError(result[2])
        return result[3]

    def script(self, script):
        return self.call("WebDriver:ExecuteScript", {"script": script, "args": [], "sandbox": None, "newSandbox": False})["value"]

    def wait(self, expression, timeout=20):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            value = self.script(f"return ({expression});")
            if value:
                return value
            time.sleep(0.05)
        raise AssertionError(f"Timed out: {expression}; state: {self.script('return window.result || null;')}")


def game_zip():
    # Original fixture: draw color 42, transmit a note, busy-loop indefinitely.
    program = bytes.fromhex("b81300cd10b800a08ec031ffb900fab02af3aa ba3103 b03fee ba3003 ec b090ee b045ee b064ee ebfe")
    data = io.BytesIO()
    with zipfile.ZipFile(data, "w", zipfile.ZIP_DEFLATED) as archive:
        archive.writestr("GAME/TEST.COM", program)
    return list(data.getvalue())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--prince', action='store_true', help='also run the local, ignored target/prince-game.zip')
    parser.add_argument('--benchmark', action='store_true', help='pair saved baseline and current Worker WASM on the local PRINCE ZIP; no audio/rendering')
    parser.add_argument('--benchmark-baseline', type=Path, default=ROOT / 'target/perf-baseline',
                        help='directory containing reference doslet_web_worker.js and its WASM')
    args = parser.parse_args()
    bridge_path = next(DIST.glob("snippets/*/src/browser/bridge.js")).relative_to(DIST)
    page = f"""<!doctype html><html><head><base href="/"></head><body>
    <script type="module">
    import {{Browser}} from '/{bridge_path}';
    window.bridge = new Browser(); window.result = {{events: []}};
    setInterval(() => {{
        const packet = bridge.poll();
        if (packet.event) result.events.push(packet.event);
        if (packet.state) result.state = packet.state;
        result.audio = packet.audio;
        if (packet.frame) {{
            result.frame = {{width: packet.frame.width, height: packet.frame.height,
                colored: packet.frame.pixels.some((b, i) => i % 4 !== 3 && b !== 0)}};
            bridge.releaseFrame();
        }}
    }}, 16);
    window.startTest = () => {{
        bridge.resumeAudio().then(() => result.resumed = true).catch(e => result.error = String(e));
        bridge.importZip(new Uint8Array({game_zip()}));
    }};
    </script></body></html>""".encode()

    class Handler(http.server.SimpleHTTPRequestHandler):
        def do_GET(self):
            if (args.prince or args.benchmark) and self.path == '/prince.zip':
                data = (ROOT / 'target/prince-game.zip').read_bytes()
                self.send_response(200)
                self.send_header('Content-Type', 'application/zip')
                self.end_headers()
                self.wfile.write(data)
            elif args.benchmark and self.path.startswith('/baseline/'):
                name = self.path.removeprefix('/baseline/')
                if name not in ['doslet_web_worker.js', 'doslet_web_worker_bg.wasm']:
                    self.send_error(404); return
                data = (args.benchmark_baseline / name).read_bytes()
                self.send_response(200)
                self.send_header('Content-Type', 'application/wasm' if name.endswith('.wasm') else 'text/javascript')
                self.end_headers(); self.wfile.write(data)
            elif self.path == "/smoke.html":
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
    with socket.socket() as port:
        port.bind(("127.0.0.1", 0))
        marionette_port = port.getsockname()[1]
    with tempfile.TemporaryDirectory(prefix="doslet-firefox-") as directory:
        profile = Path(directory)
        prefs = {
            "marionette.port": marionette_port,
            "media.autoplay.default": 0,
            "media.autoplay.block-webaudio": False,
            "media.volume_scale": "0.0",
            "browser.shell.checkDefaultBrowser": False,
        }
        (profile / "user.js").write_text("\n".join(f"user_pref({json.dumps(key)}, {json.dumps(value)});" for key, value in prefs.items()))
        log_path = ROOT / "target/browser-smoke-firefox.log"
        with log_path.open("w") as log:
            firefox = subprocess.Popen(["firefox", "--headless", "--no-remote", "--marionette", "--profile", str(profile)], stdout=log, stderr=log, env={**os.environ, "MOZ_HEADLESS": "1"})
            try:
                deadline = time.monotonic() + 20
                while True:
                    try:
                        connection = socket.create_connection(("127.0.0.1", marionette_port), timeout=30)
                        break
                    except ConnectionRefusedError:
                        if time.monotonic() > deadline or firefox.poll() is not None:
                            raise RuntimeError(f"Firefox did not start; see {log_path}")
                        time.sleep(0.1)
                driver = Marionette(connection)
                driver.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {"acceptInsecureCerts": True}}})
                url = f"http://127.0.0.1:{server.server_port}"
                driver.call("WebDriver:Navigate", {"url": url + "/"})
                driver.wait("document.querySelectorAll('canvas').length > 0")
                print("iced Canvas2D startup: OK", flush=True)
                driver.call("WebDriver:Navigate", {"url": url + "/smoke.html"})
                driver.wait("window.bridge && bridge.ready")
                driver.script("startTest(); return true;")
                driver.wait("result.resumed || result.error")
                assert not driver.script("return result.error || null;"), driver.script("return result.error;")
                driver.wait("result.events.some(e => e.kind === 'imported')")
                driver.script("bridge.command('start-game', 'GAME/TEST.COM'); return true;")
                driver.wait("result.audio.frames > 4800 && result.audio.peak > 0.01 && result.frame?.colored")
                print("worker ZIP + guest MIDI -> Rust/WASM worklet + pixels: OK", flush=True)
                before = driver.script("return {frames: result.audio.frames, steps: result.state.steps};")
                driver.script("const end = performance.now() + 800; while (performance.now() < end) {} return true;")
                driver.wait(f"result.audio.frames > {before['frames'] + 12000} && result.state.steps > {before['steps']}")
                print("audio and machine advance while the window is blocked: OK", flush=True)
                driver.script("bridge.command('pause', ''); return true;")
                driver.wait("result.state.status === 2")
                time.sleep(0.1)
                paused = driver.script("return {frames: result.audio.frames, steps: result.state.steps};")
                time.sleep(0.15)
                assert driver.script("return {frames: result.audio.frames, steps: result.state.steps};") == paused
                driver.script("bridge.command('run', ''); return true;")
                driver.wait(f"result.audio.frames > {paused['frames'] + 1000}")
                driver.script("bridge.command('reset', ''); return true;")
                driver.wait("result.state.status === 1 && result.audio.notes === 1 && result.state.steps < 2000000")
                assert not driver.script("return result.events.some(e => ['error', 'audio-error'].includes(e.kind));"), driver.script("return result.events;")
                print("pause/resume/reset and bounded channels: OK", flush=True)
                # Mock only the external device boundary. This tests real browser
                # framing, scheduling and switching without opening hardware MIDI.
                driver.script("""
                    window.sent = [];
                    window.mockOutput = {id: 'test-port', name: 'Test synth', state: 'connected',
                        open: () => Promise.resolve(),
                        send: (bytes, when) => sent.push({bytes: Array.from(bytes), when})};
                    Object.defineProperty(navigator, 'requestMIDIAccess', {configurable: true,
                        value: () => Promise.resolve({outputs: new Map([['test-port', mockOutput]])})});
                    bridge.listMidi().then(ports => bridge.selectMidi(ports[0]))
                        .then(() => result.hostSelected = true).catch(e => result.error = String(e));
                    return true;
                """)
                driver.wait("result.hostSelected || result.error")
                assert not driver.script("return result.error || null;")
                driver.wait("sent.some(m => m.bytes[0] === 0x90 && m.bytes[1] === 69 && m.bytes[2] === 100 && typeof m.when === 'number')")
                driver.script("""
                    const send = mockOutput.send;
                    mockOutput.send = () => { throw new Error('Test device disconnected'); };
                    bridge.send({kind: 'resync-audio'});
                    window.restoreMock = () => { mockOutput.send = send; };
                    return true;
                """)
                driver.wait("result.events.some(e => e.kind === 'error' && e.error.includes('Test device disconnected'))")
                driver.script("restoreMock(); bridge.selectMidi('0: Test synth').then(() => { bridge.command('run', ''); result.hostRecovered = true; }); return true;")
                driver.wait("result.hostRecovered && result.state.status === 1")
                driver.script("result.events = []; return true;")
                driver.script("bridge.selectMidi('Rust synth').then(() => result.rustSelected = true).catch(e => result.error = String(e)); return true;")
                driver.wait("result.rustSelected || result.error")
                assert not driver.script("return result.error || null;")
                driver.wait("result.audio.state === 'running' && result.audio.peak > 0.01")
                print("Web MIDI device mock: complete messages, timestamps and backend replay: OK", flush=True)
                if args.benchmark:
                    driver.script("""
                        bridge.command('pause', '');
                        result.bench = [];
                        const source = `
                            self.onmessage = async ({data}) => {
                                try {
                                    const {default: init, WorkerEngine} = await import(data.module);
                                    const wasm = await init(); const engine = new WorkerEngine();
                                    const bytes = new Uint8Array(await (await fetch(data.zip)).arrayBuffer());
                                    engine.import_zip(bytes); engine.start_game('PRINCE/PRINCE.EXE');
                                    const started = performance.now(); let midi = 0, midiHash = 2166136261, timeHash = 2166136261;
                                    while (engine.steps() < 300000000 && engine.status() === 1) {
                                        engine.advance(10000); const count = engine.drain_midi(); midi += count;
                                        if (count) {
                                            for (const byte of new Uint8Array(wasm.memory.buffer, engine.midi_ptr(), count)) {
                                                midiHash = Math.imul(midiHash ^ byte, 16777619) >>> 0;
                                            }
                                            const time = engine.nanos();
                                            timeHash = Math.imul(timeHash ^ (time >>> 0), 16777619) >>> 0;
                                            timeHash = Math.imul(timeHash ^ Math.floor(time / 4294967296), 16777619) >>> 0;
                                        }
                                        engine.drain_pcm();
                                    }
                                    const ms = performance.now() - started;
                                    const count = engine.convert_frame(); let hash = 2166136261;
                                    for (const byte of new Uint8Array(wasm.memory.buffer, engine.frame_ptr(), count)) {
                                        hash = Math.imul(hash ^ byte, 16777619) >>> 0;
                                    }
                                    self.postMessage({ms, steps: engine.steps(), nanos: engine.nanos(),
                                        status: engine.status(), midi, midiHash, timeHash, width: engine.width(), height: engine.height(), hash});
                                } catch (error) { self.postMessage({error: String(error)}); }
                            };`;
                        window.benchURL = URL.createObjectURL(new Blob([source], {type: 'text/javascript'}));
                        window.runBench = variant => {
                            const worker = new Worker(benchURL, {type: 'module'});
                            worker.onmessage = ({data}) => { result.bench.push({variant, ...data}); worker.terminate(); };
                            worker.postMessage({module: new URL(variant === 'baseline' ? '/baseline/doslet_web_worker.js' : '/browser/doslet_web_worker.js', location.href).href,
                                zip: new URL('/prince.zip', location.href).href});
                        };
                        return true;
                    """)
                    runs = []
                    for index, variant in enumerate(['baseline', 'current', 'current', 'baseline', 'baseline', 'current']):
                        driver.script(f"runBench({json.dumps(variant)}); return true;")
                        driver.wait(f"result.bench.length > {index}", timeout=120)
                        run = driver.script(f"return result.bench[{index}];")
                        assert 'error' not in run, run
                        assert run['steps'] == 300000000 and run['status'] == 1 and run['midi'] == 2228, run
                        if runs:
                            for key in ['nanos', 'hash', 'midiHash', 'timeHash', 'width', 'height']:
                                assert run[key] == runs[0][key], (key, run, runs[0])
                        runs.append(run)
                        print('WASM CPU benchmark: ' + json.dumps(run), flush=True)
                    (ROOT / 'target/prince-wasm-benchmark.json').write_text(json.dumps(runs, indent=2))
                if args.prince:
                    driver.script("""
                        result.events = [];
                        fetch('/prince.zip').then(r => r.arrayBuffer()).then(bytes => bridge.importZip(new Uint8Array(bytes)));
                        return true;
                    """)
                    driver.wait("result.events.some(e => e.kind === 'imported')")
                    driver.script("bridge.command('start-game', 'PRINCE/PRINCE.EXE'); return true;")
                    driver.wait("result.state.status === 1 && result.state.steps >= 60000000 && result.frame.width === 640 && result.frame.colored", timeout=90)
                    print('real PRINCE in WASM worker: startup graphics at 60M steps: OK', flush=True)
                    driver.wait("result.state.status === 1 && result.state.steps >= 300000000 && result.frame.width === 640 && result.audio.notes >= 269 && result.audio.peak > 0.01", timeout=180)
                    print('real PRINCE in WASM worker: 300M steps, 640x480 and synthesized guest MIDI: OK', flush=True)
                print(json.dumps(driver.script("return result;")), flush=True)
                driver.call("WebDriver:DeleteSession")
                connection.close()
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
