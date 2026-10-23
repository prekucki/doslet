#!/usr/bin/env python3
"""Exercise doslet-web serve, bundled launch and AOT reset in real Firefox.

Uses the existing stdlib Marionette driver; no Node, WebDriver package or audio
listening claim. Build target/debug/doslet-web first.
"""
import argparse
import json
import os
from pathlib import Path
import queue
import re
import socket
import subprocess
import tempfile
import threading
import time
from browser_smoke import Marionette

ROOT = Path(__file__).resolve().parents[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("config", type=Path)
    parser.add_argument("--expect-aot", action="store_true")
    parser.add_argument("--aot-off", action="store_true")
    parser.add_argument("--expect-procedure-aot", action="store_true")
    parser.add_argument("--wasm-procedures", type=Path)
    parser.add_argument("--aot-profile", type=Path)
    parser.add_argument("--observe-seconds", type=float, default=0)
    parser.add_argument("--web-dist", type=Path)
    parser.add_argument("--mode", choices=["simple", "aot", "v86"])
    parser.add_argument("--speed-tiers", type=Path)
    args = parser.parse_args()
    if args.mode and (args.aot_off or args.expect_aot or args.wasm_procedures):
        parser.error("--mode cannot be combined with legacy AOT options")
    command = [str(ROOT / "target/debug/doslet-web"), "serve", str(args.config), "--port", "0", "--offline"]
    if args.web_dist:
        command += ["--web-dist", str(args.web_dist)]
    if args.mode:
        command += ["--mode", args.mode]
    if args.speed_tiers:
        command += ["--speed-tiers", str(args.speed_tiers)]
    if args.aot_profile:
        command += ["--aot-profile", str(args.aot_profile)]
    if args.expect_aot:
        command += ["--aot", "required"]
    if args.aot_off:
        command += ["--aot", "off"]
    if args.wasm_procedures:
        command += ["--wasm-procedures", str(args.wasm_procedures), "--aot", "required"]
    with tempfile.TemporaryDirectory(prefix="doslet-bundle-firefox-") as directory:
        profile = Path(directory)
        with (ROOT / "target/web-bundle-smoke.log").open("w") as log:
            preview = subprocess.Popen(command, cwd=ROOT, stdout=subprocess.PIPE, stderr=log, text=True)
            lines = queue.Queue()
            def read_lines():
                for line in preview.stdout:
                    lines.put(line)
                lines.put(None)
            threading.Thread(target=read_lines, daemon=True).start()
            firefox = None
            try:
                deadline = time.monotonic() + 180
                url = None
                while time.monotonic() < deadline:
                    line = lines.get(timeout=max(1, deadline - time.monotonic()))
                    if line is None:
                        raise RuntimeError("Preview failed; see target/web-bundle-smoke.log")
                    match = re.search(r"http://localhost:\d+/", line)
                    if match:
                        url = match[0]
                        break
                if not url:
                    raise RuntimeError("Preview did not start within 180 seconds")
                print(f"Rust serve built and started: {url}", flush=True)
                with socket.socket() as port:
                    port.bind(("127.0.0.1", 0))
                    marionette_port = port.getsockname()[1]
                prefs = {"marionette.port": marionette_port, "media.autoplay.default": 0,
                         "media.autoplay.block-webaudio": False, "media.volume_scale": "0.0",
                         "browser.shell.checkDefaultBrowser": False}
                (profile / "user.js").write_text("\n".join(f"user_pref({json.dumps(k)}, {json.dumps(v)});" for k, v in prefs.items()))
                firefox = subprocess.Popen(["firefox", "--headless", "--no-remote", "--marionette", "--profile", str(profile)],
                                           stdout=log, stderr=log, env={**os.environ, "MOZ_HEADLESS": "1"})
                deadline = time.monotonic() + 20
                while True:
                    try:
                        connection = socket.create_connection(("127.0.0.1", marionette_port), timeout=30)
                        break
                    except ConnectionRefusedError:
                        if time.monotonic() > deadline or firefox.poll() is not None:
                            raise RuntimeError("Firefox failed; see target/web-bundle-smoke.log")
                        time.sleep(0.1)
                driver = Marionette(connection)
                driver.call("WebDriver:NewSession", {"capabilities": {"alwaysMatch": {"acceptInsecureCerts": True}}})
                driver.call("WebDriver:Navigate", {"url": url})
                driver.wait("window.dosletBundleBridge && !document.getElementById('doslet-bundle-launch').disabled", timeout=60)
                driver.script("window.bundleError=null;window.result={get state(){return dosletBundleBridge.state},get error(){return window.bundleError}};window.addEventListener('doslet-bundle-status',e=>{if(e.detail.error)window.bundleError=e.detail.error});return true;")
                element = driver.call("WebDriver:FindElement", {"using": "css selector", "value": "#doslet-bundle-launch"})["value"]
                driver.call("WebDriver:ElementClick", {"id": element["element-6066-11e4-a52e-4f735466cecf"]})
                driver.wait("!document.getElementById('doslet-bundle-cover') || window.bundleError", timeout=120)
                assert not driver.script("return window.bundleError;"), driver.script("return window.bundleError;")
                print("Bundled game launch completed", flush=True)
                driver.wait("dosletBundleBridge.state?.steps > 1000", timeout=60)
                if args.speed_tiers:
                    driver.wait("dosletBundleBridge.state?.cpuMode === 'tiers' && dosletBundleBridge.state.tierRates.length === 3")
                if args.mode:
                    driver.wait(f"dosletBundleBridge.state?.cpuCore === {json.dumps(args.mode)}")
                if args.mode == "v86":
                    driver.wait("dosletBundleBridge.state?.v86?.jitEntries > 0", timeout=120)
                    print("v86 generated WASM code executed", flush=True)
                if args.expect_aot or args.mode == "aot":
                    driver.wait("dosletBundleBridge.state?.aotTranslatedSteps > 0", timeout=120)
                    assert driver.script("return dosletBundleBridge.state.aot.active && dosletBundleBridge.state.aot.installed && dosletBundleBridge.state.aot.grants > 0;")
                    print("Generated AOT code executed", flush=True)
                if args.expect_procedure_aot:
                    driver.wait("dosletBundleBridge.state?.aot?.installed && dosletBundleBridge.state.aot.entries > 0", timeout=120)
                    print("Procedure AOT module installed", flush=True)
                if args.observe_seconds:
                    time.sleep(args.observe_seconds)
                    print("Observed before reset: " + json.dumps(driver.script("return {steps:dosletBundleBridge.state.steps,aot:dosletBundleBridge.state.aot,fault:dosletBundleBridge.state.fault,videoMode:dosletBundleBridge.state.videoMode};")), flush=True)
                driver.script("dosletBundleBridge.command('pause','');return true;")
                driver.wait("dosletBundleBridge.state?.status === 2")
                before = driver.script("return dosletBundleBridge.state.steps;")
                time.sleep(0.15)
                assert before == driver.script("return dosletBundleBridge.state.steps;")
                driver.script("dosletBundleBridge.command('reset','');return true;")
                driver.wait("dosletBundleBridge.state?.status === 1 && dosletBundleBridge.state.steps > 1000", timeout=60)
                if args.speed_tiers:
                    assert driver.script("return dosletBundleBridge.state.cpuMode === 'tiers' && dosletBundleBridge.state.tierRates.length === 3;")
                if args.mode == "v86":
                    driver.wait("dosletBundleBridge.state?.cpuCore === 'v86' && dosletBundleBridge.state.v86?.jitEntries > 0", timeout=120)
                if args.expect_aot or args.mode == "aot":
                    driver.wait("dosletBundleBridge.state?.aotTranslatedSteps > 0", timeout=120)
                if args.expect_procedure_aot:
                    driver.wait("dosletBundleBridge.state?.aot?.installed && dosletBundleBridge.state.aot.entries > 0", timeout=120)
                result = driver.script("return {status:dosletBundleBridge.state.status,steps:dosletBundleBridge.state.steps,core:dosletBundleBridge.state.cpuCore,aot:dosletBundleBridge.state.aot,v86:dosletBundleBridge.state.v86,fault:dosletBundleBridge.state.fault};")
                assert not result["fault"], result
                print(json.dumps(result), flush=True)
                print("Rust serve + bundled launch + pause/reset in Firefox: OK", flush=True)
                driver.call("WebDriver:DeleteSession")
                connection.close()
            finally:
                for process in [firefox, preview]:
                    if process is not None:
                        process.terminate()
                        try:
                            process.wait(timeout=10)
                        except subprocess.TimeoutExpired:
                            process.kill()
                            process.wait()


if __name__ == "__main__":
    main()
