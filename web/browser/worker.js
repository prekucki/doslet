import init, { WorkerEngine } from './doslet_web_worker.js';

const wasm = await init();
const engine = new WorkerEngine();
let port = null, audioRunning = false, epoch = 0, played = 0;
let clockMode = 'worklet', nextPoll = 0;
let ended = false;
let anchor = null, nextVideo = 0, failed = false;
let snapshotPending = null;
// Transferable host presentation buffers. Guest RAM and VRAM never cross realms.
let midiPool = Array.from({length: 16}, () => new ArrayBuffer(1024));
const videoPool = Array.from({length: 2}, () => new ArrayBuffer(640 * 480 * 4));
// setTimeout(0) acquires a minimum delay in nested timer chains. A message task
// yields to commands without imposing that extra delay when the CPU is behind.
const wake = new MessageChannel();
wake.port1.onmessage = () => tick();

function report(error) {
    engine.pause(); failed = true;
    if (port) port.postMessage({kind: 'active', epoch, active: false});
    self.postMessage({kind: 'error', error: String(error)});
    present(true);
}
function sendBytes(nanos, bytes) {
    const buffer = midiPool.pop();
    if (!buffer) throw new Error('MIDI transfer queue full');
    new Uint8Array(buffer).set(bytes);
    port.postMessage({kind: 'midi', epoch, nanos, count: bytes.length, buffer}, [buffer]);
}
function sendSnapshot() {
    while (snapshotPending && midiPool.length) {
        const {bytes, offset, nanos} = snapshotPending;
        const end = Math.min(offset + 1024, bytes.length);
        sendBytes(nanos, bytes.subarray(offset, end));
        snapshotPending.offset = end;
        if (end === bytes.length) snapshotPending = null;
    }
}
function syncAudio(replay = false) {
    epoch++; played = engine.nanos(); anchor = null; ended = false;
    snapshotPending = null;
    if (!port) return;
    port.postMessage({kind: 'reset', epoch, nanos: played, active: engine.status() === 1});
    if (audioRunning && replay) {
        const count = engine.snapshot_midi();
        snapshotPending = {bytes: new Uint8Array(wasm.memory.buffer, engine.replay_ptr(), count).slice(), offset: 0, nanos: played};
        sendSnapshot();
    }
}
function drain() {
    const count = engine.drain_midi();
    const bytes = new Uint8Array(wasm.memory.buffer, engine.midi_ptr(), count);
    if (audioRunning && port && count) sendBytes(engine.nanos(), bytes);
    engine.drain_pcm(); // SB PCM mixing remains explicitly unsupported.
}
function present(force = false) {
    const now = performance.now();
    if (!force && now < nextVideo) return;
    nextVideo = now + 16;
    const state = {kind: 'state', epoch, status: engine.status(), steps: engine.steps(),
        nanos: engine.nanos(), cs: engine.cs(), ip: engine.ip(), ax: engine.ax(),
        cx: engine.cx(), di: engine.di(), fault: engine.fault()};
    self.postMessage(state);
    if (!videoPool.length) return;
    const count = engine.convert_frame();
    if (!count) return;
    let buffer = videoPool.pop();
    if (buffer.byteLength < count) buffer = new ArrayBuffer(count);
    new Uint8Array(buffer, 0, count).set(new Uint8Array(wasm.memory.buffer, engine.frame_ptr(), count));
    self.postMessage({kind: 'frame', epoch, width: engine.width(), height: engine.height(), count, buffer}, [buffer]);
}
self.onmessage = ({data}) => {
    try {
        if (data.kind === 'video-return') { videoPool.push(data.buffer); return; }
        if (data.kind === 'connect') {
            if (port) port.close();
            port = data.port; clockMode = data.mode || 'worklet';
            // A newly connected output recovers host/audio faults without
            // recreating the machine. Session::start still rejects CPU faults.
            failed = false;
            midiPool = Array.from({length: 16}, () => new ArrayBuffer(1024));
            const connected = port;
            port.onmessage = ({data: message}) => {
                if (connected !== port) return;
                if (message.kind === 'return') { midiPool.push(message.buffer); sendSnapshot(); }
                if (message.kind === 'progress' && message.epoch === epoch) played = message.nanos;
                if (message.kind === 'error') report(message.error);
            };
            syncAudio(audioRunning); return;
        }
        if (data.kind === 'audio-running') {
            if (audioRunning !== data.running) {
                audioRunning = data.running;
                syncAudio(audioRunning);
            }
            return;
        }
        if (data.kind === 'resync-audio') { syncAudio(audioRunning); return; }
        if (data.kind === 'import') {
            engine.pause(); anchor = null;
            if (port) port.postMessage({kind: 'active', epoch, active: false});
            try {
                const programs = engine.import_zip(new Uint8Array(data.buffer)).split('\n');
                self.postMessage({kind: 'imported', programs});
            } catch (error) { self.postMessage({kind: 'import-error', error: String(error)}); }
        } else if (data.kind === 'start-game') {
            engine.start_game(data.program); failed = false; syncAudio();
            self.postMessage({kind: 'started', name: data.program.split('/').pop()});
        } else if (data.kind === 'reset') {
            engine.reset(); failed = false; syncAudio();
            self.postMessage({kind: 'reset-done'});
        } else if (data.kind === 'pause') {
            engine.pause(); anchor = null;
            if (port) port.postMessage({kind: 'active', epoch, active: false});
        } else if (data.kind === 'run') {
            if (failed) throw new Error('Reset the session after an audio/worker fault');
            engine.start(); anchor = null;
            if (port && clockMode === 'midi') syncAudio(audioRunning);
            else if (port) port.postMessage({kind: 'active', epoch, active: engine.status() === 1});
        } else if (data.kind === 'step') {
            if (audioRunning && !midiPool.length) throw new Error('MIDI transfer queue full');
            engine.single_step(); drain();
            if (port && audioRunning) port.postMessage({kind: 'commit', epoch, nanos: engine.nanos()});
        }
        present(true);
    } catch (error) { report(error); }
};
function tick() {
    try {
        let behind = false;
        if (engine.status() === 1 && !failed && !snapshotPending) {
            const now = performance.now();
            if (!anchor) anchor = {host: now, guest: engine.nanos()};
            const wallTarget = anchor.guest + (now - anchor.host) * 1_000_000;
            const target = audioRunning && port ? (clockMode === 'worklet' ? played + 80_000_000 : wallTarget + 80_000_000) : wallTarget;
            let budget = 400_000;
            // Yield at least every ~8 ms of host work, as well as at the finite
            // step limit. Presentation frequency never advances guest devices.
            while (engine.nanos() < target && budget > 0 && engine.status() === 1 && performance.now() - now < 8) {
                if (audioRunning && !midiPool.length) break;
                const completed = engine.advance(Math.min(budget, 10000));
                budget -= completed; drain();
                if (!completed) break;
            }
            if (audioRunning && port) port.postMessage({kind: 'commit', epoch, nanos: engine.nanos()});
            if (engine.status() !== 1 && !ended && port && audioRunning) {
                port.postMessage({kind: 'end', epoch}); ended = true;
                port.postMessage({kind: 'poll', epoch});
            }
            if (audioRunning && port && clockMode === 'worklet' && now >= nextPoll) {
                port.postMessage({kind: 'poll', epoch}); nextPoll = now + 20;
            }
            behind = engine.nanos() < target;
        }
        present();
        if (behind && midiPool.length) wake.port2.postMessage(null);
        else setTimeout(tick, 4);
    } catch (error) { report(error); setTimeout(tick, 16); }
}
self.postMessage({kind: 'ready'});
present(true);
tick();
