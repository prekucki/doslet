import init, { WorkerEngine } from './doslet_web_worker.js';
import {HostIo} from './io.js';
import {instantiateWasmCore, instantiateWasmProcedures} from './wasm-core.js';

const wasm = await init();
const engine = new WorkerEngine();
let aot = null, aotAssets = null;
async function prepareAot(assets) {
    if (!assets) return null;
    if (assets.kind === 'procedure-memory')
        return instantiateWasmProcedures(new Uint8Array(assets.module), wasm.memory,
            metadata => engine.validate_procedure_aot(metadata));
    const profile = assets.profile ? new Uint8Array(assets.profile) : null;
    return instantiateWasmCore(new Uint8Array(assets.module), {
        abi_ptr: () => engine.aot_abi_ptr(),
        abi_len: () => engine.aot_abi_len(),
        poll: (completed, remaining) => engine.aot_poll(completed, remaining),
        validate_profile: profile => engine.validate_aot(profile),
    }, wasm.memory, profile);
}
const io = new HostIo(engine, error => self.postMessage({kind:'storage-error',error}));
let transitioning = false, wasWaiting = false;
let startupKeys = '', startupIndex = 0;
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
let hostSpeed = {at: performance.now(), guest: engine.nanos(), busy: 0, calm: 0};
function resetHostSpeed() { hostSpeed = {at: performance.now(), guest: engine.nanos(), busy: 0, calm: 0}; }
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
function syncAudio(replay = false, resetReceiver = false) {
    epoch++; played = engine.nanos(); anchor = null; ended = false;
    resetHostSpeed();
    snapshotPending = null;
    if (!port) return;
    audioMessage({kind: 'reset', epoch, nanos: played, active: engine.status() === 1, resetReceiver});
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
    const state = {kind: 'state', epoch, status: engine.status(), steps: engine.steps(), cpuMhz: engine.cpu_mhz(), effectiveCpuMhz: engine.effective_cpu_mhz(), hostCpuLimit: engine.host_cpu_limit(), cpuMode: engine.cpu_mode(), tierRates: Array.from(engine.tier_rates()), tierCounts: Array.from(engine.tier_counts(), Number), keyboardMode: engine.keyboard_mode(),
        nanos: engine.nanos(), idleNanos: engine.idle_nanos(), waitingForIo:engine.waiting_for_io(), dirty:engine.dirty_overlay(), pendingCommit:engine.pending_commit(), cs: engine.cs(), ip: engine.ip(), ax: engine.ax(),
        cx: engine.cx(), di: engine.di(), fault: engine.fault(), midiMode: engine.midi_mode(),
        midiUpgraded: engine.midi_upgraded(), midiSysex: engine.midi_sysex(),
        audioUse: engine.audio_use(), guestPcmRate: engine.guest_pcm_rate(), guestPcmBits: engine.guest_pcm_bits(),
        guestPcmChannels: engine.guest_pcm_channels(),
        midiBytes: engine.midi_bytes(), midiNotes: engine.midi_notes(), midiChannels: engine.midi_channels(), diskReadBytes: engine.disk_read_bytes(),
        pointerInUse: engine.pointer_in_use(), pointerRelative: engine.pointer_relative_in_use(), memory: JSON.parse(engine.memory_usage()), videoMode: engine.video_mode_label(),
        aotTranslatedSteps: Number(engine.aot_translated_steps()),
        aot: {active: !!aot, ...JSON.parse(aot?.kind === 'procedure-memory' ?
            engine.procedure_aot_diagnostics() : engine.aot_diagnostics())},
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
        if(data.kind==='bundle-import') {
            if(transitioning) throw new Error('Machine preparation is still in progress');
            const programs=engine.import_zip(new Uint8Array(data.buffer)).split('\n');
            self.postMessage({kind:'call-result',id:data.id,programs});
            self.postMessage({kind:'imported',programs});
            return;
        }
        if (data.kind === 'audio-hardware') {
            try {
                if (transitioning) throw new Error('Machine preparation is still in progress');
                engine.configure_unused_audio(data.program);
                self.postMessage({kind:'audio-hardware-applied'});
            } catch (error) {
                self.postMessage({kind:'audio-hardware-failed',message:String(error)});
            }
            present(true); return;
        }
        if(transitioning && !['video-return','connect','audio-running','resync-audio'].includes(data.kind)) throw new Error('Machine preparation is still in progress');
        if (data.kind === 'key') {
            engine.key_event(data.scan, data.ascii, data.modifiers, data.pressed);
            return;
        }
        if (data.kind === 'pointer') { engine.pointer_event(data.x,data.y,data.button,data.pressed); return; }
        if (data.kind === 'pointer-relative') { engine.pointer_relative(data.dx,data.dy); return; }
        if (data.kind === 'midi-mode') { engine.set_midi_mode(data.program); syncAudio(audioRunning); present(true); return; }
        if (data.kind === 'cpu-mhz') { engine.set_cpu_mhz(Number(data.program)); anchor = null; present(true); return; }
        if (data.kind === 'cpu-settings') {
            const tierMatch = /^tiers:([0-9]+):([0-9]+):([0-9]+)$/.exec(data.program);
            if (tierMatch) {
                engine.set_tier_settings(...tierMatch.slice(1).map(Number));
                resetHostSpeed(); anchor = null; present(true); return;
            }
            const match = /^(fixed|adaptive):([0-9]+)$/.exec(data.program);
            if (!match) throw new Error('Invalid CPU settings');
            engine.set_cpu_settings(match[1], Number(match[2]));
            resetHostSpeed();
            anchor = null; present(true); return;
        }
        if (data.kind === 'mouse-mode') { engine.set_mouse_mode(data.program); present(true); return; }
        if (data.kind === 'keyboard-mode') { engine.set_keyboard_mode(data.program); present(true); return; }
        if (data.kind === 'video-return') { videoPool.push(data.buffer); return; }
        if (data.kind === 'connect') {
            if (port) port.close();
            if (audioPort) audioPort.close();
            port = data.port; clockMode = data.mode || 'worklet';
            audioPort = data.audioPort || null;
            engine.set_output_mode(data.dialect || 'gm');
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
            syncAudio(audioRunning, true); return;
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
            resetHostSpeed();
            audioMessage({kind: 'active', epoch, active: false});
        } else if (data.kind === 'run') {
            if (failed) throw new Error('Reset the session after an audio/worker fault');
            engine.start(); anchor = null;
            resetHostSpeed();
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
        if(data.kind==='apply-machine' && data.bundleRuntime) engine.prepare_bundle_runtime();
        prepared=await io.prepare(metadata,reset);
        if(data.kind==='import-saves') engine.candidate_import(new Uint8Array(data.buffer));
        if(data.kind==='apply-machine' && data.seedBuffer && prepared.emptyStore)
            engine.candidate_import(new Uint8Array(data.seedBuffer));
        if(data.kind==='apply-machine' && data.tierProfile)
            engine.prepare_candidate_tiers(data.tierProfile);
        if(data.kind==='apply-machine') preparedOutput=await prepareOutput(metadata.output);
        const nextAssets = reset ? aotAssets : (data.aot || null);
        const nextAot = await prepareAot(nextAssets);
        await io.install(prepared);
        prepared=null;
        aot = null; aotAssets = null;
        if (nextAot) {
            if (nextAot.kind === 'procedure-memory') {
                engine.install_procedure_aot(nextAot.metadata, nextAot.call);
                aot = {kind: nextAot.kind, run: budget => engine.advance(budget)};
            } else {
                engine.install_aot(nextAot.profile);
                aot = nextAot;
            }
            aotAssets = nextAssets;
        }
        if(data.kind==='apply-machine' && data.seedBuffer && io.namespace && engine.dirty_overlay()) {
            engine.pause();engine.request_save_commit();await io.settled();
            if(engine.pending_commit() || engine.dirty_overlay()) throw new Error('Bundled saves could not be committed');
            engine.start();
        }
        if(io.namespace && data.kind==='apply-machine') self.postMessage({kind:'storage-status-request'});
        const resetReceiver=preparedOutput===null; // A replacement initializes its new port on connect.
        if(preparedOutput!==null) {self.postMessage({kind:'commit-output',token:preparedOutput});preparedOutput=null;}
        failed=false;syncAudio(false, resetReceiver);
        if(data.kind==='apply-machine') {
            startupKeys = data.startupKeys || '';
            startupIndex = 0;
        } else if(data.kind==='reset') startupIndex = 0;
        if(data.kind==='import-saves') {
            engine.pause();engine.request_save_commit();await io.settled();
            if(engine.pending_commit() || (io.namespace && engine.dirty_overlay())) throw new Error('Imported saves remain dirty; persistent commit failed');
            reply(null);
        } else if(data.kind==='apply-machine') {
            self.postMessage({kind:'machine-applied',message:'Machine configuration applied',output:metadata.output,profile:data.profile});
            reply(null);
        }
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
        if(waiting || wasWaiting!==waiting) { anchor=null; resetHostSpeed(); }
        wasWaiting=waiting;
        let behind = false;
        if (engine.status() === 1 && !failed && !snapshotPending && !transitioning && !waiting) {
            if (startupIndex < startupKeys.length && engine.waiting_for_key()) {
                // Queue the whole sequence while the first BIOS read is blocked.
                // Later prompts may poll the buffer without blocking again.
                while (startupIndex < startupKeys.length) {
                    const ascii = startupKeys.charCodeAt(startupIndex);
                    if (!engine.startup_key(ascii)) break;
                    startupIndex++;
                }
            }
            const now = performance.now();
            if (!anchor) anchor = {host: now, guest: engine.nanos()};
            const wallTarget = anchor.guest + (now - anchor.host) * 1_000_000;
            const target = audioRunning && port ? (clockMode === 'worklet' || audioPort ? played + 80_000_000 : wallTarget + 80_000_000) : wallTarget;
            let budget = 400_000;
            // Yield at least every ~8 ms of host work, as well as at the finite
            // step limit. Presentation frequency never advances guest devices.
            while (engine.nanos() < target && budget > 0 && engine.status() === 1 && performance.now() - now < 8) {
                if (audioRunning && midiPool.length < 12) break;
                const chunk = Math.min(budget, 10000);
                if (aot) engine.aot_begin();
                const completed = aot ? aot.run(chunk) : engine.advance(chunk);
                budget -= completed; drain();
                io.pump();
                if(engine.waiting_for_io()) {anchor=null;break;}
                if (!completed || engine.timeslice_yielded()) break;
            }
            hostSpeed.busy += performance.now() - now;
            if (audioRunning && port) audioMessage({kind: 'commit', epoch, nanos: engine.nanos()});
            if (engine.status() !== 1 && !ended && port && audioRunning) {
                audioMessage({kind: 'end', epoch}); ended = true;
                audioMessage({kind: 'poll', epoch});
            }
            if (audioRunning && port && (clockMode === 'worklet' || audioPort) && now >= nextPoll) {
                audioMessage({kind: 'poll', epoch}); nextPoll = now + 20;
            }
            behind = engine.nanos() < target && !engine.waiting_for_io();
            // Adaptive mode caps the rate at host capacity, separately from the
            // user's ceiling. The cap recovers gradually after sustained headroom,
            // so a loading spike does not keep the game slow afterwards.
            if (engine.cpu_mode() === 'adaptive' && now - hostSpeed.at >= 250) {
                const guestElapsed = engine.nanos() - hostSpeed.guest;
                const hostElapsed = (now - hostSpeed.at) * 1_000_000;
                const ceiling = engine.cpu_mhz(), limit = engine.host_cpu_limit() || ceiling;
                let calm = 0;
                if (behind && guestElapsed < hostElapsed * 0.9) {
                    const current = engine.effective_cpu_mhz();
                    const next = Math.max(1, Math.floor(current * guestElapsed / hostElapsed * 0.85));
                    if (next < limit) engine.set_host_cpu_mhz(next);
                } else if (!behind && hostSpeed.busy < (now - hostSpeed.at) * 0.5 && limit < ceiling) {
                    calm = hostSpeed.calm + 1;
                    if (calm >= 4) {
                        engine.set_host_cpu_mhz(Math.min(ceiling, Math.ceil(limit * 1.1) + 1));
                        calm = 0;
                    }
                }
                hostSpeed = {at: now, guest: engine.nanos(), busy: 0, calm};
            }
        }
        present();
        if (behind && midiPool.length >= 12) wake.port2.postMessage(null);
        else setTimeout(tick, 4);
    } catch (error) { report(error); setTimeout(tick, 16); }
}
self.postMessage({kind: 'ready'});
present(true);
tick();
