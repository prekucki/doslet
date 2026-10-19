import './worklet-codec.js';
import { initSync, AudioEngine } from './doslet_web_audio.js';

class DosletProcessor extends AudioWorkletProcessor {
    constructor(options) {
        super();
        let stage = 'WASM initialization';
        try {
        // The module was compiled on the window thread. Instantiation and every
        // buffer allocation finish here, before process() renders any audio.
        this.wasm = initSync({module: options.processorOptions.module});
        stage = 'AudioEngine allocation';
        this.engine = new AudioEngine(sampleRate);
        if (options.processorOptions.control && options.processorOptions.pcm) {
            stage = 'Roland ROM setup';
            const result = this.engine.set_mt32(options.processorOptions.control, options.processorOptions.pcm,
                !!options.processorOptions.gm);
            if (result) throw new Error(`Unsupported Roland ROM pair (${result})`);
        }
        stage = 'buffer views';
        this.refreshViews();
        this.epoch = -1; this.active = false; this.failed = false;
        this.warming = true; this.portToWorker = null;
        this.port.onmessage = ({data}) => {
            if (data.kind === 'select-synth') {
                try {
                    if (data.selection === 'Rust synth') this.engine.set_rust_synth();
                    else {
                        const result = this.engine.set_mt32(data.control, data.pcm, !!data.gm);
                        if (result) throw new Error(`Unsupported Roland ROM pair (${result})`);
                    }
                    this.active = false;
                    this.engine.clear_spectrum();
                    this.port.postMessage({kind: 'synth-selected', id: data.id});
                } catch (error) {
                    this.port.postMessage({kind: 'synth-failed', id: data.id, error: String(error)});
                }
                return;
            }
            if (data.kind === 'disconnect') {
                this.active = false;
                this.engine.clear_spectrum();
                if (this.portToWorker) this.portToWorker.close();
                this.portToWorker = null;
            }
            if (data.kind === 'connect') {
                if (this.portToWorker) this.portToWorker.close();
                this.portToWorker = data.port;
                const source = data.port;
                source.onmessage = event => this.receive(event.data, source);
            }
        };
        this.port.postMessage({kind: 'ready'});
        } catch (error) {
            this.active = false; this.failed = true;
            this.port.postMessage({kind: 'error', error: `AudioWorklet setup (${stage}): ${String(error)} ${error.stack || ''}`});
        }
    }
    fail(error) {
        this.failed = true;
        const detail = error?.stack || String(error);
        this.port.postMessage({kind: 'error', error: detail});
        if (this.portToWorker) this.portToWorker.postMessage({kind: 'error', error: detail});
    }
    refreshViews() {
        const buffer = this.wasm.memory.buffer;
        if (this.midi?.buffer === buffer) return;
        this.left = new Float32Array(buffer, this.engine.left_ptr(), this.engine.capacity());
        this.right = new Float32Array(buffer, this.engine.right_ptr(), this.engine.capacity());
        this.midi = new Uint8Array(buffer, this.engine.midi_ptr(), 1024);
        this.pcm = new Float64Array(buffer, this.engine.pcm_ptr(), 128 * 4);
    }
    receive(data, source = this.portToWorker) {
        try {
            if (source !== this.portToWorker) return;
            if (data.kind === 'reset') {
                this.epoch = data.epoch; this.engine.reset(data.nanos);
                this.refreshViews();
                this.active = data.active; this.failed = false; this.warming = true; this.baseGuest = data.nanos;
            } else if (data.epoch === this.epoch && !this.failed) {
                if (data.kind === 'active') {
                    this.active = data.active;
                    if (!this.active) this.engine.clear_spectrum();
                }
                if (data.kind === 'end') this.warming = false;
                if (data.kind === 'poll') {
                    const message = {kind: 'progress', epoch: this.epoch, nanos: this.engine.nanos(),
                        frames: this.engine.frames(), notes: this.engine.notes(),
                        peak: this.engine.peak(), underruns: this.engine.underruns()};
                    message.pcmFrames = this.engine.pcm_frames(); message.pcmNonzero = this.engine.pcm_nonzero();
                    message.spectrum = Array.from({length: 16}, (_, i) => this.engine.spectrum_band(i));
                    this.port.postMessage(message); this.portToWorker.postMessage(message);
                }
                if (data.kind === 'midi') {
                    for (let offset = 0; offset < data.count; offset += 1024) {
                        this.refreshViews();
                        const count = Math.min(1024, data.count - offset);
                        this.midi.set(new Uint8Array(data.buffer, offset, count));
                        const result = this.engine.receive(data.nanos, count);
                        if (result) { this.fail(`WASM MIDI queue error ${result}`); break; }
                    }
                }
                if (data.kind === 'pcm') {
                    this.refreshViews();
                    if (!Number.isInteger(data.count) || data.count < 0 || data.count > 128) throw new Error('Invalid PCM packet size');
                    this.pcm.set(new Float64Array(data.buffer, 0, data.count * 4));
                    const result = this.engine.receive_pcm(data.count);
                    if (result) this.fail(`WASM PCM queue error ${result}`);
                }
                if (data.kind === 'commit') {
                    const result = this.engine.receive(data.nanos, 0);
                    if (result) this.fail(`WASM MIDI watermark error ${result}`);
                    if (data.nanos - this.baseGuest >= 40_000_000) this.warming = false;
                }
            }
        } catch (error) { this.fail(error); }
        finally {
            if (data.buffer && source) source.postMessage({kind: 'return', buffer: data.buffer}, [data.buffer]);
        }
    }
    process(_inputs, outputs) {
        const channels = outputs[0];
        if (!channels || !channels.length || !this.active || this.failed || this.warming) return true;
        try {
            this.refreshViews();
            // Honor the actual quantum length. Views were allocated at setup;
            // fixed Rust buffers and the queue do not allocate or grow memory.
            let offset = 0;
            while (offset < channels[0].length) {
                const count = Math.min(this.left.length, channels[0].length - offset);
                const result = this.engine.render(count);
                if (result) { this.fail(`WASM synth error ${result}`); break; }
                for (let i = 0; i < count; i++) {
                    channels[0][offset + i] = this.left[i];
                    if (channels[1]) channels[1][offset + i] = this.right[i];
                }
                offset += count;
            }
        } catch (error) { this.fail(error); }
        return true;
    }
}
registerProcessor('doslet-midi', DosletProcessor);
