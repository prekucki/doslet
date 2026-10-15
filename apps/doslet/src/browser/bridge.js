// Browser API glue. The window owns presentation; it never owns a PC or synth.
export class Browser {
    constructor() {
        this.assets = new URL('browser/', document.baseURI);
        this.worker = new Worker(new URL('worker.js', this.assets), {type: 'module'});
        this.ready = false; this.queue = []; this.events = [];
        this.state = null; this.frame = null;
        this.context = null; this.node = null; this.gain = null;
        this.initializing = null; this.muted = false;
        this.backend = 'Rust synth'; this.connectedBackend = null;
        this.midiAccess = null; this.midiLabels = new Map(); this.hostMidi = null;
        this.stats = {frames: 0, notes: 0, peak: 0, underruns: 0};
        this.mouseCanvas = null; this.mouseWanted = false; this.mousePending = false;
        this.padGeneration = new Map(); this.padSuppressed = new Map(); this.padSequence = 0;
        this.padInFlight = false; this.inputEnabled = true; this.pendingCalls = new Map(); this.nextCall = 1;
        this.heldKeys = new Set(); this.suppressedKeys = new Set();
        this.preparedOutputs = new Map();
        window.addEventListener('gamepadconnected', event => {
            this.padGeneration.set(event.gamepad.index, (this.padGeneration.get(event.gamepad.index) || 0) + 1);
        });
        window.addEventListener('gamepaddisconnected', event => {
            this.padGeneration.set(event.gamepad.index, (this.padGeneration.get(event.gamepad.index) || 0) + 1);
            this.releaseInput();
        });
        document.addEventListener('visibilitychange', () => { if (document.hidden) this.releaseInput(); });
        document.addEventListener('pointerlockchange', () => {
            if (this.mouseCaptured() && !this.mouseWanted) document.exitPointerLock();
            if (!this.mouseCaptured()) {
                this.mouseWanted = false;
                this.send({kind:'pointer',x:0,y:0,button:-2,pressed:false});
            }
            this.mousePending = false;
        });
        document.addEventListener('pointerlockerror', () => {
            this.mouseWanted = false; this.mousePending = false;
            this.events.push({kind:'error',error:'Mouse capture unavailable: browser rejected pointer lock'});
        });
        document.addEventListener('mousemove', event => {
            if (!this.mouseCaptured()) return;
            this.send({kind:'pointer-relative',dx:event.movementX,dy:event.movementY});
            event.stopImmediatePropagation();
        }, true);
        for (const kind of ['mousedown','mouseup']) document.addEventListener(kind, event => {
            if (!this.mouseCaptured()) return;
            const button = [0,2,1][event.button];
            if (button !== undefined) this.send({kind:'pointer',x:0,y:0,button,pressed:kind === 'mousedown'});
            event.preventDefault(); event.stopImmediatePropagation();
        }, true);
        document.addEventListener('contextmenu', event => {
            if (this.mouseCaptured()) event.preventDefault();
        });
        document.addEventListener('keydown', event => {
            if ((this.mouseCaptured() || this.mousePending) && event.key === 'Escape') {
                this.releaseMouse(); event.stopImmediatePropagation();
            }
        }, true);
        window.addEventListener('blur', () => { this.releaseMouse(); this.releaseInput(); });
        this.worker.onerror = event => {
            const error=event.message || 'Machine Worker failed';
            for(const call of this.pendingCalls.values()) {clearTimeout(call.timer);call.reject(new Error(error));}
            this.pendingCalls.clear();this.events.push({kind:'machine-failed',message:error,error});
        };
        this.worker.onmessage = ({data}) => {
            if(data.kind==='storage-status-request') {
                (async()=>{
                    let persisted=false;
                    try {persisted=await navigator.storage?.persisted() || await navigator.storage?.persist() || false;}catch {}
                    let estimate={};try{estimate=await navigator.storage?.estimate() || {};}catch {}
                    this.events.push({kind:'storage-policy',persisted,...estimate,message:persisted?'Browser saves: durable storage granted':'Browser saves: best effort; keep exported backups'});
                })();return;
            }
            if(data.kind === 'prepare-output') {
                this.prepareOutput(data.output).then(prepared=>{
                    this.preparedOutputs.set(data.token,prepared);this.send({kind:'output-prepared',token:data.token});
                }).catch(error=>this.send({kind:'output-prepared',token:data.token,error:String(error)}));
                return;
            }
            if(data.kind === 'commit-output') {
                const prepared=this.preparedOutputs.get(data.token);this.preparedOutputs.delete(data.token);
                if(prepared) this.commitOutput(prepared);return;
            }
            if(data.kind === 'discard-output') {this.preparedOutputs.delete(data.token);return;}
            if (data.kind === 'input-ack') { this.padInFlight = false; return; }
            if (data.kind === 'call-result') {
                const call = this.pendingCalls.get(data.id); this.pendingCalls.delete(data.id);
                if (call) {clearTimeout(call.timer);data.error ? call.reject(new Error(data.error)) : call.resolve(data);}
                return;
            }
            if (data.kind === 'ready') {
                this.ready = true;
                for (const [message, transfer] of this.queue) this.worker.postMessage(message, transfer);
                this.queue = [];
                console.info('Doslet machine ready in Web Worker');
            } else if (data.kind === 'state') {
                this.state = data;
                if (data.status !== 1 && this.mouseWanted) this.releaseMouse();
            }
            else if (data.kind === 'frame') {
                if (this.frame) this.returnFrame(this.frame);
                this.frame = data;
            } else { if(this.events.length >= 256) this.events.shift(); this.events.push(data); }
        };
    }
    send(message, transfer = []) {
        if (this.ready) this.worker.postMessage(message, transfer);
        else {
            if (this.queue.length >= 32) throw new Error('Worker startup command queue full');
            this.queue.push([message, transfer]);
        }
    }
    command(kind, program) {
        if (['pause','reset','import','start'].includes(kind)) { this.releaseMouse(); this.releaseInput(); }
        this.send({kind, program});
    }
    applyMachine(profile, program, discardChanges = false) {
        this.releaseInput(); this.releaseMouse(); this.send({kind:'apply-machine', profile, program, discardChanges});
    }
    physicalKey(usage, pressed, enabled=true) {
        if (!pressed) { this.heldKeys.delete(usage); this.suppressedKeys.delete(usage); }
        else { if (this.heldKeys.has(usage)) return; this.heldKeys.add(usage); }
        if(pressed && !enabled) this.suppressedKeys.add(usage);
        if (!this.suppressedKeys.has(usage)) this.send({kind:'physical-key', usage, pressed});
    }
    joystick(x, y, buttons) { this.send({kind:'joystick', x, y, buttons}); }
    releaseInput() {
        for (const usage of this.heldKeys) this.suppressedKeys.add(usage);
        for (const pad of navigator.getGamepads?.() || []) if (pad) this.padSuppressed.set(pad.index, {
            buttons: pad.buttons.map(button => button.pressed), axes: pad.axes.map(value => Math.abs(value) > 0.12),
        });
        this.send({kind:'release-input'});
    }
    inputCapture(enabled) { if (this.inputEnabled && !enabled) this.releaseInput(); this.inputEnabled = enabled; }
    sampleGamepads() {
        const pads = [...(navigator.getGamepads?.() || [])].filter(Boolean).slice(0, 16);
        if (!this.padInFlight) {
            const enabled=this.inputEnabled && document.hasFocus() && !document.hidden && this.state?.status === 1;
            const states = pads.map(pad => {
                const blocked = this.padSuppressed.get(pad.index);
                const axes = pad.axes.slice(0, 8).map((value, index) => {
                    if (blocked?.axes[index]) { if (Math.abs(value) <= 0.12) blocked.axes[index] = false; return 0; }
                    return Math.round(Math.max(-1, Math.min(1, value)) * 32767);
                });
                let buttons = 0;
                pad.buttons.slice(0,32).forEach((button,index) => {
                    if (blocked?.buttons[index]) { if (!button.pressed) blocked.buttons[index] = false; }
                    else if (button.pressed) buttons = (buttons | (1 << index)) >>> 0;
                });
                return {id:pad.index,identity:pad.id,generation:this.padGeneration.get(pad.index)||1,axes,buttons};
            });
            this.padInFlight = true;
            this.send({kind:'gamepads', enabled, sequence:++this.padSequence, states});
        }
        return pads.map(pad=>({id:pad.id,index:pad.index,connected:pad.connected}));
    }
    pickFile(accept) {
        return new Promise((resolve, reject) => {
            const input = document.createElement('input'); input.type = 'file'; input.accept = accept;
            input.onchange = () => resolve(input.files?.[0] || null);
            input.oncancel = () => resolve(null);
            try { input.click(); } catch(error) { reject(error); }
        });
    }
    async chooseProfile() {
        const file = await this.pickFile('.toml,text/plain');
        if (!file) return null;
        if (file.size > 1024 * 1024) throw new Error('Profile exceeds 1 MiB');
        return file.text();
    }
    download(bytes, name, type) {
        const url = URL.createObjectURL(new Blob([bytes], {type}));
        const link = document.createElement('a'); link.href = url; link.download = name; link.click();
        setTimeout(()=>URL.revokeObjectURL(url), 1000);
    }
    downloadProfile(text) { this.download(text, 'doslet-machine.toml', 'application/toml'); }
    async chooseIso() { const blob = await this.pickFile('.iso,application/x-iso9660-image'); if (blob) await this.call('import-iso', {blob}); }
    importIso(bytes) { this.send({kind:'import-iso',blob:new Blob([bytes])}); }
    call(kind, fields = {}, transfer = []) {
        if (this.pendingCalls.size >= 8) return Promise.reject(new Error('Too many pending storage commands'));
        const id = this.nextCall++;
        return new Promise((resolve,reject)=> {
            const timer=setTimeout(()=>{this.pendingCalls.delete(id);reject(new Error('Storage operation timed out; export current changes before restarting'));},120000);
            this.pendingCalls.set(id,{resolve,reject,timer});this.send({kind,id,...fields},transfer);
        });
    }
    async exportSaves() { const result = await this.call('export-saves'); this.download(result.buffer,'doslet-saves.dsv','application/octet-stream'); }
    async importSaves() {
        const file = await this.pickFile('.dsv,application/octet-stream'); if (!file) return;
        if (file.size > 16*1024*1024) throw new Error('Save import exceeds 16 MiB');
        const buffer = await file.arrayBuffer(); await this.call('import-saves',{buffer},[buffer]);
    }
    async prepareOutput(output) {
        if(!output.audio) return {disabled:true};
        const selection=output.midi_output;
        if(selection==='apple') throw new Error('Apple MIDI output is available only on native macOS; choose rust or an exposed MIDI port in the browser');
        if(selection==='moont') throw new Error('CM-32L ROM synthesis is available on desktop; choose rust or an exposed MIDI port in the browser');
        if(selection==='rust') {await this.startRustAudio();return {selection:'Rust synth',output};}
        const id=this.midiLabels.get(selection);
        if(!id) throw new Error('Find MIDI outputs and choose an available port before applying the profile');
        if(output.host_midi==='mt32' && !this.midiAccess?.sysexEnabled) throw new Error('Select the MT-32 MIDI port with SysEx permission before applying this profile');
        const device=this.midiAccess.outputs.get(id);
        if(!device || device.state!=='connected') throw new Error('Selected MIDI output is disconnected');
        const {HostMidi}=await import(new URL('host-midi.js',this.assets));
        await device.open();await this.startRustAudio();
        return {selection,device,HostMidi,output};
    }
    commitOutput(prepared) {
        if(prepared.disabled) {
            this.muted=true;if(this.gain)this.gain.gain.value=0;if(this.hostMidi)this.hostMidi.mute(true);
            this.send({kind:'audio-running',running:false});return;
        }
        if(this.hostMidi) {this.hostMidi.close();this.hostMidi=null;}
        this.muted=false;this.gain.gain.value=1;this.backend=prepared.selection;
        if(prepared.selection==='Rust synth') {this.attachWorklet();this.syncAudioState();return;}
        const pcm=new MessageChannel(),midi=new MessageChannel();
        this.node.port.postMessage({kind:'connect',port:pcm.port1},[pcm.port1]);
        this.hostMidi=new prepared.HostMidi(prepared.device,midi.port1,error=>this.events.push({kind:'audio-error',error}),prepared.output.host_midi==='mt32');
        this.connectedBackend='midi';
        this.send({kind:'connect',mode:'midi',dialect:prepared.output.host_midi,port:midi.port2,audioPort:pcm.port2},[midi.port2,pcm.port2]);
        this.syncAudioState();
    }
    mouseCaptured() { return !!this.mouseCanvas && document.pointerLockElement === this.mouseCanvas; }
    captureMouse() {
        if (this.mouseCaptured() || this.mousePending) return;
        const canvas = document.querySelector('canvas');
        if (!canvas?.requestPointerLock) throw new Error('Browser pointer lock unavailable');
        this.mouseCanvas = canvas; this.mouseWanted = true; this.mousePending = true;
        let request;
        try { request = canvas.requestPointerLock(); }
        catch (error) { this.mouseWanted = false; this.mousePending = false; throw error; }
        if (request?.catch) request.catch(error => {
            if (!this.mouseWanted) return;
            this.mouseWanted = false; this.mousePending = false;
            this.events.push({kind:'error',error:`Mouse capture: ${error}`});
        });
    }
    releaseMouse() {
        this.mouseWanted = false; this.mousePending = false;
        if (this.mouseCaptured()) document.exitPointerLock();
        this.send({kind:'pointer',x:0,y:0,button:-2,pressed:false});
    }
    keyEvent(scan, ascii, modifiers, pressed) { this.send({kind: 'key', scan, ascii, modifiers, pressed}); }
    pointerEvent(x,y,button,pressed) {
        if (!this.mouseCaptured() && !this.mousePending) this.send({kind:'pointer',x,y,button,pressed});
    }
    importZip(bytes) {
        // A setup transfer of compressed input. The worker then owns the source
        // and decompressed drive; no game data is retained in window state.
        const buffer = bytes.slice().buffer;
        this.send({kind: 'import', buffer}, [buffer]);
    }
    poll() {
        const data = {state: this.state, gamepads:this.sampleGamepads(), mouseCaptured: this.mouseCaptured(), event: this.events.shift() || null,
            audio: {state: this.backend === 'Rust synth' ? this.context?.state || 'inactive' : 'MIDI port', ready: !!this.node,
                muted: this.muted, ...this.stats}};
        if (this.hostMidi) data.audio = {...this.stats, state: 'MIDI port', ready: true, muted: this.muted,
            messages: this.hostMidi.messages, notes: this.hostMidi.notes, late: this.hostMidi.late};
        if (this.frame) {
            const frame = this.frame;
            // wasm-bindgen copies this view into iced-owned RGBA storage before
            // releaseFrame returns the transport buffer to the worker pool.
            data.frame = {width: frame.width, height: frame.height,
                pixels: new Uint8Array(frame.buffer, 0, frame.count)};
        }
        return data;
    }
    releaseFrame() {
        if (this.frame) { this.returnFrame(this.frame); this.frame = null; }
    }
    returnFrame(frame) { this.worker.postMessage({kind: 'video-return', buffer: frame.buffer}, [frame.buffer]); }
    toggleMute() {
        this.muted = !this.muted;
        if (this.hostMidi) {
            this.hostMidi.mute(this.muted);
            if (!this.muted) this.send({kind: 'resync-audio'});
        }
        if (this.gain) this.gain.gain.value = this.muted ? 0 : 1;
    }
    resumeAudio() {
        if (this.hostMidi) { this.syncAudioState(); return Promise.resolve(); }
        return this.startRustAudio().then(() => {
            this.attachWorklet(); this.syncAudioState();
        });
    }
    startRustAudio() {
        // This part executes synchronously inside the user's button handler.
        if (!this.context) {
            this.context = new AudioContext({sampleRate: 48000});
            if (this.context.sampleRate !== 48000) throw new Error('Web Audio did not accept 48000 Hz');
            if (!this.context.audioWorklet) throw new Error('AudioWorklet unavailable; use localhost or HTTPS');
            this.gain = this.context.createGain();
            this.gain.connect(this.context.destination);
            this.context.onstatechange = () => this.syncAudioState();
        }
        const resumed = this.context.resume();
        if (!this.node && !this.initializing) {
            this.initializing = this.initializeAudio().finally(() => { this.initializing = null; });
        }
        return Promise.all([resumed, this.initializing]);
    }
    syncAudioState() {
        this.send({kind: 'audio-running', running: this.hostMidi ? true : !!this.node && this.context.state === 'running'});
    }
    attachWorklet() {
        if (this.connectedBackend === 'worklet') return;
        const channel = new MessageChannel();
        this.node.port.postMessage({kind: 'connect', port: channel.port1}, [channel.port1]);
        this.send({kind: 'connect', mode: 'worklet', port: channel.port2}, [channel.port2]);
        this.connectedBackend = 'worklet';
    }
    async listMidi() {
        if (!navigator.requestMIDIAccess) throw new Error('Web MIDI is unavailable in this browser');
        // Permission is requested only by the explicit Find MIDI outputs button.
        this.midiAccess = await navigator.requestMIDIAccess({sysex: false});
        this.midiAccess.onstatechange = () => {
            if (this.hostMidi?.output.state === 'disconnected') {
                this.events.push({kind: 'audio-error', error: 'MIDI output disconnected; choose the built-in synth or another output'});
                this.send({kind: 'audio-running', running: false});
            }
        };
        this.midiLabels.clear();
        let index = 0;
        for (const output of this.midiAccess.outputs.values()) {
            if (output.state === 'connected') this.midiLabels.set(`${index++}: ${output.name || output.id}`, output.id);
        }
        if (!this.midiLabels.size) throw new Error('No MIDI output ports. A system synthesizer must expose a MIDI port.');
        return [...this.midiLabels.keys()];
    }
    selectMidi(selection, dialect = 'gm') {
        if (selection === 'Rust synth') {
            // Resume remains synchronous with this user gesture, before await.
            return this.startRustAudio().then(() => {
                if (this.hostMidi) { this.hostMidi.close(); this.hostMidi = null; }
                this.backend = selection; this.muted = false;
                this.gain.gain.value = 1;
                this.attachWorklet(); this.syncAudioState();
            });
        }
        return this.selectHostMidi(selection, dialect);
    }
    async selectHostMidi(selection, dialect = 'gm') {
        let audioReady;
        const pcmFailure = error => {
            this.events.push({kind: 'audio-error', error: `Sound Blaster output unavailable: ${error}`});
            return false;
        };
        // Keep host MIDI usable when Web Audio is unavailable, with an explicit notice.
        try { audioReady = this.startRustAudio().then(() => true).catch(pcmFailure); }
        catch (error) { audioReady = Promise.resolve(pcmFailure(error)); }
        if (dialect === 'mt32') {
            this.midiAccess = await navigator.requestMIDIAccess({sysex: true});
            if (!this.midiAccess.sysexEnabled) throw new Error('MT-32 output requires Web MIDI SysEx permission');
        }
        const id = this.midiLabels.get(selection);
        const output = this.midiAccess?.outputs.get(id);
        if (!output || output.state !== 'connected') throw new Error('MIDI output unavailable; refresh the output list');
        const {HostMidi} = await import(new URL('host-midi.js', this.assets));
        await output.open();
        const pcmReady = await audioReady;
        if (this.hostMidi) this.hostMidi.close();
        let pcmChannel = null;
        if (pcmReady) {
            this.gain.gain.value = 1; // The worklet keeps playing Sound Blaster PCM.
            pcmChannel = new MessageChannel();
            this.node.port.postMessage({kind: 'connect', port: pcmChannel.port1}, [pcmChannel.port1]);
        } else if (this.node) this.node.port.postMessage({kind: 'disconnect'});
        const channel = new MessageChannel();
        this.hostMidi = new HostMidi(output, channel.port1,
            error => this.events.push({kind: 'audio-error', error}), dialect === 'mt32');
        this.backend = selection; this.muted = false; this.connectedBackend = 'midi';
        this.send({kind: 'connect', mode: 'midi', dialect, port: channel.port2, audioPort: pcmChannel?.port2}, pcmChannel ? [channel.port2, pcmChannel.port2] : [channel.port2]);
        this.syncAudioState();
    }
    async initializeAudio() {
        const response = await fetch(new URL('doslet_web_audio_bg.wasm', this.assets));
        if (!response.ok) throw new Error(`Audio WASM HTTP ${response.status}`);
        const module = await WebAssembly.compile(await response.arrayBuffer());
        await this.context.audioWorklet.addModule(new URL('worklet.js', this.assets));
        const node = new AudioWorkletNode(this.context, 'doslet-midi', {
            numberOfInputs: 0, numberOfOutputs: 1, outputChannelCount: [2],
            processorOptions: {module},
        });
        try {
            await new Promise((resolve, reject) => {
                const timeout = setTimeout(() => reject(new Error('AudioWorklet startup timed out')), 15000);
                node.onprocessorerror = () => {
                    clearTimeout(timeout);
                    this.events.push({kind: 'audio-error', error: 'AudioWorklet processor failed'});
                    this.send({kind: 'audio-running', running: false});
                    if (this.node === node) { this.node = null; this.connectedBackend = null; node.disconnect(); }
                    reject(new Error('AudioWorklet processor failed'));
                };
                node.port.onmessage = ({data}) => {
                    if (data.kind === 'ready') { clearTimeout(timeout); resolve(); }
                    if (data.kind === 'progress') this.stats = data;
                    if (data.kind === 'error') { this.events.push({kind: 'audio-error', error: data.error}); clearTimeout(timeout); reject(new Error(data.error)); }
                };
            });
            node.connect(this.gain);
            this.node = node;
            console.info('Doslet MIDI ready in AudioWorklet (Rust/WASM)');
        } catch (error) {
            node.disconnect(); node.port.close(); throw error;
        }
    }
}
