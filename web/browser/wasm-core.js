// ABI v2 bridge. The generated module imports the worker's memory and the
// fixed address of its small CPU/dispatch scratch, so polls copy nothing.
// The Rust host keeps exclusive ownership of guest RAM and devices.
// host: {abi_ptr(), abi_len(), poll(completed, remaining)}; memory: host WASM memory.
export async function instantiateWasmCore(bytes, host, memory, expectedProfile = null) {
    const module = await WebAssembly.compile(bytes);
    const sections = WebAssembly.Module.customSections(module, 'doslet.profile');
    const embedded = sections.length === 1 ? new Uint8Array(sections[0]) : null;
    if (!embedded || embedded.length > 64*1024*1024)
        throw new Error('Missing or oversized embedded AOT profile');
    if (expectedProfile) {
        if (!embedded || embedded.length !== expectedProfile.length ||
            embedded.some((byte, index) => byte !== expectedProfile[index]))
            throw new Error('AOT module and runtime profile differ');
    }
    const abi = WebAssembly.Module.customSections(module, 'doslet.abi');
    const version = abi.length === 1 && abi[0].byteLength === 4 ? new DataView(abi[0]).getUint32(0, true) : 0;
    if (version !== 2) {
        throw new Error(`Unsupported Doslet WASM core ABI ${version}; rebuild the bundle with this doslet-web`);
    }
    host.validate_profile?.(embedded);
    const size = host.abi_len(), ptr = host.abi_ptr();
    if (size !== 65600) throw new Error('Invalid Doslet WASM core scratch size');
    if (!ptr || ptr % 4 || ptr + size > memory.buffer.byteLength) throw new Error('Invalid Doslet WASM core scratch');
    const base = new WebAssembly.Global({value: 'i32', mutable: false}, ptr | 0);
    const instance = await WebAssembly.instantiate(module, {doslet: {
        memory, base,
        poll: (completed, remaining) => host.poll(completed >>> 0, remaining >>> 0),
    }});
    let running = false, failed = false;
    return {
        profile: embedded,
        run(budget) {
            if (!Number.isInteger(budget) || budget < 0 || budget > 0xffffffff) throw new Error('Invalid WASM core budget');
            if (running || failed) throw new Error('WASM core unavailable after an error or during execution');
            running = true;
            try { return instance.exports.run(budget) >>> 0; }
            catch (error) { failed = true; throw error; }
            finally { running = false; }
        }
    };
}

// The native and browser procedure paths consume the same generated module.
// Its only import is the worker's linear memory, so guest RAM and VGA writes
// stay in that backing while the Rust session owns the execution boundary.
export async function instantiateWasmProcedures(bytes, memory, validateMetadata) {
    const module = await WebAssembly.compile(bytes);
    const sections = WebAssembly.Module.customSections(module, 'doslet.procedures');
    if (sections.length !== 1 || sections[0].byteLength > 16*1024*1024)
        throw new Error('Missing or oversized embedded procedure metadata');
    const metadata = new Uint8Array(sections[0]);
    const manifest = JSON.parse(new TextDecoder('utf-8', {fatal: true}).decode(metadata));
    const functions = manifest.functions;
    if (!Array.isArray(functions) || functions.length === 0 || functions.length > 4096)
        throw new Error('Invalid procedure export list');
    const ids = functions.map(entry => entry.id);
    if (ids.some(id => !Number.isInteger(id) || id < 0 || id > 0xffffffff) ||
        new Set(ids).size !== ids.length)
        throw new Error('Invalid procedure export ID');
    ids.sort((left, right) => left - right);
    validateMetadata(metadata);
    const instance = await WebAssembly.instantiate(module, {doslet_static: {memory}});
    const exports = ids.map(id => instance.exports[`memory_procedure_${id}`]);
    if (exports.some(entry => typeof entry !== 'function'))
        throw new Error('Missing procedure export');
    let failed = false;
    return {
        kind: 'procedure-memory', metadata,
        call(index, framePointer) {
            if (failed || !Number.isInteger(index) || index < 0 || index >= exports.length ||
                !Number.isInteger(framePointer) || framePointer < 0)
                throw new Error('Invalid procedure call');
            try { return exports[index](framePointer) >>> 0; }
            catch (error) { failed = true; throw error; }
        },
    };
}
