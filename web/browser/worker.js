import init, { WorkerEngine } from './doslet_web_worker.js';
import {HostIo} from './io.js';

const wasm = await init();
const engine = new WorkerEngine();
const io = new HostIo(engine, error => self.postMessage({kind:'storage-error',error}));
let transitioning = false, wasWaiting = false;
let outputToken=0;
const outputPreparations=new Map();
function prepareOutput(output) {
    const token=++outputToken;
    return new Promise((resolve,reject)=>{
        const timer=setTimeout(()=>{outputPreparations.delete(token);reject(new Error('Host output preparation timed out'));},20000);
        outputPreparations.set(token,{resolve:()=>{clearTimeout(timer);resolve(token);},reject:error=>{clearTimeout(timer);reject(new Error(error));}});
        self.postMessage({kind:'prepare-output',token,output});
    });
}
let port = null, audioPort = null, audioRunning = false, epoch = 0, played = 0;
let clockMode = 'worklet', nextPoll = 0;
let ended = false;
let anchor = null, nextVideo = 0, failed = false;
let snapshotPending = null;
// Transferable host presentation buffers. Guest RAM and VRAM never cross realms.
let midiPool = Array.from({length: 32}, () => new ArrayBuffer(16384));
const videoPool = Array.from({length: 2}, () => new ArrayBuffer(640 * 480 * 4));
// setTimeout(0) acquires a minimum delay in nested timer chains. A message task
// yields to commands without imposing that extra delay when the CPU is behind.
const wake = new MessageChannel();
wake.port1.onmessage = () => tick();

function report(error) {
    engine.pause(); failed = true;
    if (port) port.postMessage({kind: 'active', epoch, active: false});
    if (audioPort) audioPort.postMessage({kind: 'active', epoch, active: false});
    self.postMessage({kind: 'error', error: String(error)});
    present(true);
}
function sendBytes(nanos, bytes) {
    const buffer = midiPool.pop();
    if (!buffer) throw new Error('MIDI transfer queue full');
    new Uint8Array(buffer).set(bytes);
    port.postMessage({kind: 'midi', epoch, nanos, count: bytes.length, buffer}, [buffer]);
}
function sendPcm(count) {
    const buffer = midiPool.pop();
    if (!buffer) throw new Error('Audio transfer queue full');
    new Float64Array(buffer, 0, count * 4).set(new Float64Array(wasm.memory.buffer, engine.pcm_ptr(), count * 4));
    (audioPort || port).postMessage({kind: 'pcm', epoch, count, buffer}, [buffer]);
}
function audioMessage(message) {
    if (port) port.postMessage(message);
    if (audioPort) audioPort.postMessage(message);
}
function sendSnapshot() {
    while (snapshotPending && midiPool.length) {
        const {bytes, offset, nanos} = snapshotPending;
        const end = Math.min(offset + 16384, bytes.length);
        sendBytes(nanos, bytes.subarray(offset, end));
        snapshotPending.offset = end;
        if (end === bytes.length) snapshotPending = null;
    }
}
function syncAudio(replay = false) {
    epoch++; played = engine.nanos(); anchor = null; ended = false;
    snapshotPending = null;
    if (!port) return;
    audioMessage({kind: 'reset', epoch, nanos: played, active: engine.status() === 1});
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
    for (;;) {
        const count = engine.drain_pcm();
        if (!count) break;
        if (audioRunning && port && (clockMode === 'worklet' || audioPort)) sendPcm(count);
    }
}
function present(force = false) {
    const now = performance.now();
    if (!force && now < nextVideo) return;
    nextVideo = now + 16;
    const state = {kind: 'state', epoch, status: engine.status(), steps: engine.steps(),
        nanos: engine.nanos(), waitingForIo:engine.waiting_for_io(), dirty:engine.dirty_overlay(), pendingCommit:engine.pending_commit(), cs: engine.cs(), ip: engine.ip(), ax: engine.ax(),
        cx: engine.cx(), di: engine.di(), fault: engine.fault(), midiMode: engine.midi_mode(),
        midiUpgraded: engine.midi_upgraded(), midiSysex: engine.midi_sysex(),
        midiBytes: engine.midi_bytes(), midiNotes: engine.midi_notes(),
        midiUnmapped: engine.midi_unmapped(), midiDisplay: engine.midi_display()};
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
        if(data.kind==='output-prepared') {const p=outputPreparations.get(data.token);outputPreparations.delete(data.token);if(p)data.error?p.reject(data.error):p.resolve();else self.postMessage({kind:'discard-output',token:data.token});return;}
        if(data.kind === 'gamepads') { if(!transitioning) io.gamepads(data.states,data.enabled); self.postMessage({kind:'input-ack'});return; }
        if(data.kind === 'physical-key') {if(!transitioning)engine.physical_key(data.usage,data.pressed);return;}
        if(data.kind === 'joystick') {if(!transitioning)engine.joystick(data.x,data.y,data.buttons);return;}
        if(data.kind === 'release-input') {engine.release_input();return;}
        if(['apply-machine','start-game','reset','import-saves','export-saves','import-iso'].includes(data.kind)) { hostCommand(data); return; }
        if(transitioning && !['video-return','connect','audio-running','resync-audio'].includes(data.kind)) throw new Error('Machine preparation is still in progress');
        if (data.kind === 'key') {
            engine.key_event(data.scan, data.ascii, data.modifiers, data.pressed);
            return;
        }
        if (data.kind === 'pointer') { engine.pointer_event(data.x,data.y,data.button,data.pressed); return; }
        if (data.kind === 'pointer-relative') { engine.pointer_relative(data.dx,data.dy); return; }
        if (data.kind === 'midi-mode') { engine.set_midi_mode(data.program); syncAudio(audioRunning); present(true); return; }
        if (data.kind === 'video-return') { videoPool.push(data.buffer); return; }
        if (data.kind === 'connect') {
            if (port) port.close();
            if (audioPort) audioPort.close();
            port = data.port; clockMode = data.mode || 'worklet';
            audioPort = data.audioPort || null;
            engine.set_output_mode(clockMode === 'midi' ? data.dialect || 'gm' : 'gm');
            // A newly connected output recovers host/audio faults without
            // recreating the machine. Session::start still rejects CPU faults.
            failed = false;
            midiPool = Array.from({length: 32}, () => new ArrayBuffer(16384));
            const connected = port;
            port.onmessage = ({data: message}) => {
                if (connected !== port) return;
                if (message.kind === 'return') { midiPool.push(message.buffer); sendSnapshot(); }
                if (message.kind === 'progress' && message.epoch === epoch) played = message.nanos;
                if (message.kind === 'error') report(message.error);
            };
            if (audioPort) {
                const connectedAudio = audioPort;
                audioPort.onmessage = ({data: message}) => {
                    if (connectedAudio !== audioPort) return;
                    if (message.kind === 'return') { midiPool.push(message.buffer); sendSnapshot(); }
                    if (message.kind === 'progress' && message.epoch === epoch) played = message.nanos;
                    if (message.kind === 'error') report(message.error);
                };
            }
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
            audioMessage({kind: 'active', epoch, active: false});
            try {
                const programs = engine.import_zip(new Uint8Array(data.buffer)).split('\n');
                self.postMessage({kind: 'imported', programs});
            } catch (error) { self.postMessage({kind: 'import-error', error: String(error)}); }
        } else if (data.kind === 'pause') {
            engine.pause(); anchor = null;
            audioMessage({kind: 'active', epoch, active: false});
        } else if (data.kind === 'run') {
            if (failed) throw new Error('Reset the session after an audio/worker fault');
            engine.start(); anchor = null;
            if (port && clockMode === 'midi') syncAudio(audioRunning);
            else if (port) audioMessage({kind: 'active', epoch, active: engine.status() === 1});
        } else if (data.kind === 'step') {
            if (audioRunning && midiPool.length < 12) throw new Error('Audio transfer queue full');
            engine.single_step(); drain();
            if (port && audioRunning) audioMessage({kind: 'commit', epoch, nanos: engine.nanos()});
        }
        present(true);
    } catch (error) { report(error); }
};
async function hostCommand(data) {
    let preparedOutput=null,prepared=null;
    const reply=(error,result={},transfer=[])=>{
        if(data.id) self.postMessage({kind:'call-result',id:data.id,error,...result},transfer);
        else if(error) self.postMessage({kind:data.kind==='apply-machine'?'machine-failed':'error',error,message:error});
    };
    if(transitioning) {reply('Another storage or machine operation is in progress');return;}
    if(data.kind==='import-iso') {io.selectedBlob=data.blob;reply(null);return;}
    transitioning=true;engine.release_input();anchor=null;
    try {
        await io.settled();
        if(data.kind==='export-saves') {
            const bytes=engine.export_saves();reply(null,{buffer:bytes.buffer},[bytes.buffer]);return;
        }
        const discard=data.discardChanges || data.program==='discard' || data.kind==='import-saves';
        if(engine.dirty_overlay() && !discard) throw new Error('Export unsaved changes or enable Discard temporary changes before replacing this session');
        const reset=!['apply-machine','start-game'].includes(data.kind);
        const metadata=JSON.parse(reset?engine.prepare_reset():data.kind==='start-game'?engine.prepare_start_game(data.program):engine.prepare_machine(data.profile,data.program||''));
        prepared=await io.prepare(metadata,reset);
        if(data.kind==='import-saves') engine.candidate_import(new Uint8Array(data.buffer));
        if(data.kind==='apply-machine') preparedOutput=await prepareOutput(metadata.output);
        await io.install(prepared);
        prepared=null;
        if(io.namespace && data.kind==='apply-machine') self.postMessage({kind:'storage-status-request'});
        if(preparedOutput!==null) {self.postMessage({kind:'commit-output',token:preparedOutput});preparedOutput=null;}
        failed=false;syncAudio();
        if(data.kind==='import-saves') {
            engine.pause();engine.request_save_commit();await io.settled();
            if(engine.pending_commit() || (io.namespace && engine.dirty_overlay())) throw new Error('Imported saves remain dirty; persistent commit failed');
            reply(null);
        } else if(data.kind==='apply-machine') self.postMessage({kind:'machine-applied',message:'Machine configuration applied',output:metadata.output});
        else if(data.kind==='start-game') self.postMessage({kind:'started',name:data.program.split('/').pop()});
        else self.postMessage({kind:'reset-done'});
    } catch(error) {engine.discard_machine();try{await io.discard(prepared);}catch{}if(preparedOutput!==null)self.postMessage({kind:'discard-output',token:preparedOutput});reply(String(error));}
    finally {transitioning=false;anchor=null;present(true);}
}
function tick() {
    try {
        io.pump();
        const waiting=engine.waiting_for_io();
        if(wasWaiting && !waiting && clockMode==='midi') syncAudio(audioRunning);
        if(waiting || wasWaiting!==waiting) anchor=null;
        wasWaiting=waiting;
        let behind = false;
        if (engine.status() === 1 && !failed && !snapshotPending && !transitioning && !waiting) {
            const now = performance.now();
            if (!anchor) anchor = {host: now, guest: engine.nanos()};
            const wallTarget = anchor.guest + (now - anchor.host) * 1_000_000;
            const target = audioRunning && port ? (clockMode === 'worklet' || audioPort ? played + 80_000_000 : wallTarget + 80_000_000) : wallTarget;
            let budget = 400_000;
            // Yield at least every ~8 ms of host work, as well as at the finite
            // step limit. Presentation frequency never advances guest devices.
            while (engine.nanos() < target && budget > 0 && engine.status() === 1 && performance.now() - now < 8) {
                if (audioRunning && midiPool.length < 12) break;
                const completed = engine.advance(Math.min(budget, 10000));
                budget -= completed; drain();
                io.pump();
                if(engine.waiting_for_io()) {anchor=null;break;}
                if (!completed || engine.timeslice_yielded()) break;
            }
            if (audioRunning && port) audioMessage({kind: 'commit', epoch, nanos: engine.nanos()});
            if (engine.status() !== 1 && !ended && port && audioRunning) {
                audioMessage({kind: 'end', epoch}); ended = true;
                audioMessage({kind: 'poll', epoch});
            }
            if (audioRunning && port && (clockMode === 'worklet' || audioPort) && now >= nextPoll) {
                audioMessage({kind: 'poll', epoch}); nextPoll = now + 20;
            }
            behind = engine.nanos() < target && !engine.waiting_for_io();
        }
        present();
        if (behind && midiPool.length >= 12) wake.port2.postMessage(null);
        else setTimeout(tick, 4);
    } catch (error) { report(error); setTimeout(tick, 16); }
}
self.postMessage({kind: 'ready'});
present(true);
tick();
