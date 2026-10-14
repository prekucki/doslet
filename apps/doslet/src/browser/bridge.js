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
        this.worker.onerror = event => this.events.push({kind: 'error', error: event.message || 'Worker startup failed'});
        this.worker.onmessage = ({data}) => {
            if (data.kind === 'ready') {
                this.ready = true;
                for (const [message, transfer] of this.queue) this.worker.postMessage(message, transfer);
                this.queue = [];
                console.info('Doslet machine ready in Web Worker');
            } else if (data.kind === 'state') this.state = data;
            else if (data.kind === 'frame') {
                if (this.frame) this.returnFrame(this.frame);
                this.frame = data;
            } else this.events.push(data);
        };
    }
    send(message, transfer = []) {
        if (this.ready) this.worker.postMessage(message, transfer);
        else {
            if (this.queue.length >= 32) throw new Error('Worker startup command queue full');
            this.queue.push([message, transfer]);
        }
    }
    command(kind, program) { this.send({kind, program}); }
    keyEvent(scan, ascii, modifiers, pressed) { this.send({kind: 'key', scan, ascii, modifiers, pressed}); }
    pointerEvent(x,y,button,pressed) { this.send({kind:'pointer',x,y,button,pressed}); }
    importZip(bytes) {
        // A setup transfer of compressed input. The worker then owns the source
        // and decompressed drive; no game data is retained in window state.
        const buffer = bytes.slice().buffer;
        this.send({kind: 'import', buffer}, [buffer]);
    }
    poll() {
        const data = {state: this.state, event: this.events.shift() || null,
            audio: {state: this.backend === 'Rust synth' ? this.context?.state || 'inactive' : 'MIDI port', ready: !!this.node,
                muted: this.muted, ...this.stats}};
        if (this.hostMidi) data.audio = {state: 'MIDI port', ready: true, muted: this.muted,
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
        } else if (this.gain) this.gain.gain.value = this.muted ? 0 : 1;
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
        if (dialect === 'mt32') {
            this.midiAccess = await navigator.requestMIDIAccess({sysex: true});
            if (!this.midiAccess.sysexEnabled) throw new Error('MT-32 output requires Web MIDI SysEx permission');
        }
        const id = this.midiLabels.get(selection);
        const output = this.midiAccess?.outputs.get(id);
        if (!output || output.state !== 'connected') throw new Error('MIDI output unavailable; refresh the output list');
        const {HostMidi} = await import(new URL('host-midi.js', this.assets));
        await output.open();
        if (this.hostMidi) this.hostMidi.close();
        if (this.node) this.node.port.postMessage({kind: 'disconnect'});
        if (this.gain) this.gain.gain.value = 0;
        const channel = new MessageChannel();
        this.hostMidi = new HostMidi(output, channel.port1,
            error => this.events.push({kind: 'audio-error', error}), dialect === 'mt32');
        this.backend = selection; this.muted = false; this.connectedBackend = 'midi';
        this.send({kind: 'connect', mode: 'midi', dialect, port: channel.port2}, [channel.port2]);
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
                    if (data.kind === 'progress' && !this.hostMidi) this.stats = data;
                    if (data.kind === 'error') this.events.push({kind: 'audio-error', error: data.error});
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
