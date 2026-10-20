import {HttpMedia, BlobMedia, fingerprint, hex} from './media.js';

export class HostIo {
    constructor(engine, notify) {
        this.engine = engine; this.notify = notify; this.nextId = 1;
        this.pending = new Map(); this.flights = new Set(); this.media = null; this.namespace = null;
        this.selectedBlob = null; this.activeBlob = null; this.generation = 1;
        this.input = {pad_index:0,axis_x:0,axis_y:1,button_1:0,button_2:1,deadzone_percent:12};
        this.worker = new Worker(new URL('./storage-worker.js', import.meta.url), {type:'module'});
        this.worker.onmessage = ({data}) => {
            const operation = this.pending.get(data.id); this.pending.delete(data.id);
            if (operation) {clearTimeout(operation.timer);data.ok ? operation.resolve(data) : operation.reject(Object.assign(new Error(data.error),{code:data.code}));}
        };
        this.worker.onerror = event => {
            for(const operation of this.pending.values()) {clearTimeout(operation.timer);operation.reject(new Error(event.message || 'Storage worker failed'));}
            this.pending.clear();
        };
    }
    storage(message, transfer = []) {
        if(this.pending.size >= 8) return Promise.reject(new Error('Storage command queue full'));
        return new Promise((resolve,reject)=>{
            const id = this.nextId++;
            const timer=setTimeout(()=>{this.pending.delete(id);reject(Object.assign(new Error('Storage acknowledgement timed out; commit outcome is unresolved'),{code:'Unavailable'}));},30000);
            this.pending.set(id,{resolve,reject,timer});
            this.worker.postMessage({...message,id},transfer);
        });
    }
    pump() {
        for(let index=0; index<4 && this.flights.size<4; index++) {
            const encoded = this.engine.next_io_request(); if (!encoded) break;
            const request = JSON.parse(encoded), payload = this.engine.take_io_payload();
            const generation = this.generation, media = this.media;
            const operation = (async()=>{
                try {
                    if(request.kind === 'read') {
                        if(!media) throw Object.assign(new Error('ISO source missing'),{code:'Unavailable'});
                        const bytes = await media.read(Number(request.offset),request.length);
                        if(generation===this.generation) this.engine.complete_io(encoded,bytes,'',0n);
                    } else {
                        const result = await this.storage({...request,kind:'commit',payload:payload.buffer},[payload.buffer]);
                        if(generation===this.generation) this.engine.complete_io(encoded,new Uint8Array(),'',BigInt(result.revision));
                    }
                } catch(error) {
                    if(generation===this.generation) this.engine.complete_io(encoded,new Uint8Array(),error.code || 'Unavailable',0n);
                    this.notify(`Storage: ${error}`);
                }
            })().finally(()=>this.flights.delete(operation));
            this.flights.add(operation);
        }
    }
    async settled() {
        for(let count=0;count<16;count++) {
            this.pump(); if(!this.flights.size) return;
            await Promise.all([...this.flights]);
        }
        throw new Error('Storage did not settle within the operation limit');
    }
    async prepare(metadata, reset = false) {
        let media = null, namespace = null, opened = false, emptyStore = false;
        try {
            if(metadata.iso) {
                if(reset && this.media) media = this.media;
                else if(metadata.iso.kind === 'https') media = await new HttpMedia(metadata.iso.location).prepare();
                else {
                    const blob = reset ? this.activeBlob : this.selectedBlob;
                    if(!blob) throw new Error('Choose a local ISO file before starting');
                    media = await new BlobMedia(blob).prepare();
                }
                this.engine.candidate_source(BigInt(media.length), media.validator);
            }
            if(metadata.store !== null) {
                namespace = hex(await fingerprint(metadata.store));
                const state = await this.storage({kind:'open',namespace,base:metadata.base});
                emptyStore = !state.payload?.length;
                opened = namespace !== this.namespace;
                this.engine.candidate_store(Uint8Array.from(namespace.match(/../g),v=>parseInt(v,16)), BigInt(state.fence), BigInt(state.revision), state.payload);
            }
            return {media,namespace,input:metadata.input,blob:reset?this.activeBlob:this.selectedBlob,emptyStore};
        } catch(error) {
            if(media && media!==this.media) media.close();
            if(opened) await this.storage({kind:'close',namespace});
            this.engine.discard_machine(); throw error;
        }
    }
    async install(prepared) {
        this.engine.install_machine(); this.generation++;
        if(this.media && this.media!==prepared.media) this.media.close();
        const oldNamespace=this.namespace;
        this.media=prepared.media;this.namespace=prepared.namespace;this.input=prepared.input;this.activeBlob=prepared.blob;
        if(oldNamespace && oldNamespace!==prepared.namespace) this.storage({kind:'close',namespace:oldNamespace}).catch(error=>this.notify(String(error)));
    }
    async discard(prepared) {
        if(!prepared)return;
        if(prepared.media && prepared.media!==this.media) prepared.media.close();
        if(prepared.namespace && prepared.namespace!==this.namespace) await this.storage({kind:'close',namespace:prepared.namespace});
    }
    gamepads(states, enabled) {
        const pad=states.find(pad=>this.input.pad_id?pad.identity===this.input.pad_id:pad.id===this.input.pad_index);
        if(!pad) { this.engine.release_joystick(); return; }
        const threshold=Math.round(this.input.deadzone_percent*32767/100);
        const axis=index=>{const v=pad.axes[index]||0;return Math.abs(v)<=threshold?0:Math.round(Math.sign(v)*(Math.abs(v)-threshold)*32767/(32767-threshold));};
        const buttons=((pad.buttons >>> this.input.button_1)&1)|(((pad.buttons >>> this.input.button_2)&1)<<1);
        this.engine.joystick(axis(this.input.axis_x),axis(this.input.axis_y),buttons);
        if(!enabled) this.engine.release_joystick();
    }
}
