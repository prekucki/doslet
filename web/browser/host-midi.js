// Web MIDI is an explicit alternative to Rust synthesis. It routes complete
// messages to the user's selected output; the browser supplies no GM synth.
export class Framer {
    constructor(send, allowSysex = false) {
        this.allowSysex = allowSysex; this.send = send; this.bytes = new Uint8Array(1024); this.reset();
    }
    reset() { this.status = 0; this.used = 0; this.expected = 0; this.sysex = false; }
    push(byte, when) {
        if (byte >= 0xf8) {
            if (byte === 0xf9 || byte === 0xfd) throw new Error('Undefined MIDI realtime status');
            this.send([byte], when); if (byte === 0xff) this.reset(); return;
        }
        if (this.sysex) {
            if ((byte & 0x80) && byte !== 0xf7) throw new Error('Interrupted SysEx');
            if (this.used >= this.bytes.length) throw new Error('SysEx exceeds 1024-byte limit');
            this.bytes[this.used++] = byte;
            if (byte === 0xf7) { this.send(this.bytes.subarray(0, this.used), when); this.reset(); }
            return;
        }
        if (byte & 0x80) {
            if (byte === 0xf0) {
                if (!this.allowSysex) throw new Error('SysEx is not enabled for this Web MIDI output');
                this.sysex = true; this.used = 1; this.bytes[0] = byte; return;
            }
            this.status = byte; this.used = 1; this.bytes[0] = byte;
            if (byte === 0xf6) { this.send([byte], when); this.status = 0; return; }
            if (byte < 0xf0) this.expected = (byte & 0xf0) === 0xc0 || (byte & 0xf0) === 0xd0 ? 2 : 3;
            else if (byte === 0xf1 || byte === 0xf3) this.expected = 2;
            else if (byte === 0xf2) this.expected = 3;
            else throw new Error(`Unsupported MIDI status ${byte}`);
            return;
        }
        if (!this.status) return;
        this.bytes[this.used++] = byte;
        if (this.used === this.expected) {
            this.send(this.bytes.subarray(0, this.expected), when); this.used = 1;
            if (this.status >= 0xf0) this.status = 0;
        }
    }
}

export class HostMidi {
    constructor(output, port, onError, allowSysex = false) {
        this.output = output; this.port = port; this.onError = onError;
        this.epoch = -1; this.active = false; this.muted = false;
        this.messages = 0; this.notes = 0; this.late = 0;
        this.canClear = typeof output.clear === 'function';
        this.stride = allowSysex ? 1024 : 3;
        this.pending = new Uint8Array(8192 * this.stride);
        this.lengths = new Uint16Array(8192);
        this.times = new Float64Array(8192);
        this.head = 0; this.used = 0; this.closed = false; this.failed = false;
        this.framer = new Framer((message, when) => {
            if (!this.active || this.muted) return;
            if (this.canClear) this.send(message, when);
            else {
                if (this.used === this.lengths.length) throw new Error('Host MIDI scheduling queue full');
                const index = (this.head + this.used) % this.lengths.length;
                this.pending.set(message, index * this.stride); this.lengths[index] = message.length;
                this.times[index] = when; this.used++;
            }
        }, allowSysex);
        port.onmessage = ({data}) => this.receive(data);
        if (!this.canClear) this.pump();
    }
    send(message, when) {
        this.output.send(message, when); this.messages++;
        if ((message[0] & 0xf0) === 0x90 && message[2]) this.notes++;
    }
    pump() {
        if (this.closed) return;
        try {
            const now = performance.now();
            while (this.used && this.times[this.head] <= now) {
                const index = this.head;
                this.send(this.pending.subarray(index * this.stride, index * this.stride + this.lengths[index]), this.times[index]);
                this.head = (index + 1) % this.lengths.length; this.used--;
            }
        } catch (error) {
            this.fail(error);
        }
        this.timer = setTimeout(() => this.pump(), 4);
    }
    silence() {
        this.head = 0; this.used = 0;
        if (this.canClear) this.output.clear();
        for (let c = 0; c < 16; c++) {
            this.output.send([0xb0 | c, 64, 0]);
            this.output.send([0xb0 | c, 120, 0]);
        }
    }
    fail(error) {
        if (this.failed) return;
        this.failed = true; this.active = false; this.used = 0;
        try { this.silence(); } catch (_) { /* The disconnected port already failed. */ }
        this.onError(String(error));
        this.port.postMessage({kind: 'error', error: String(error)});
    }
    receive(data) {
        try {
            if (this.closed) return;
            if (data.kind === 'reset') {
                this.failed = false;
                this.silence(); this.framer.reset(); this.epoch = data.epoch;
                this.baseGuest = data.nanos; this.baseHost = performance.now() + 40;
                this.active = data.active; this.messages = 0; this.notes = 0; this.late = 0;
            } else if (data.epoch === this.epoch && !this.failed) {
                if (data.kind === 'active') { this.active = data.active; if (!this.active) this.silence(); }
                if (data.kind === 'midi') {
                    const when = this.baseHost + (data.nanos - this.baseGuest) / 1_000_000;
                    if (when < performance.now()) this.late++;
                    for (const byte of new Uint8Array(data.buffer, 0, data.count)) this.framer.push(byte, when);
                }
            }
        } catch (error) {
            this.fail(error);
        } finally {
            if (data.buffer) this.port.postMessage({kind: 'return', buffer: data.buffer}, [data.buffer]);
        }
    }
    mute(muted) { this.muted = muted; if (muted) this.silence(); }
    close() {
        this.closed = true; clearTimeout(this.timer);
        try { this.silence(); } catch (_) { /* Closing a failed port must allow replacement. */ }
        finally { this.port.close(); }
    }
}
