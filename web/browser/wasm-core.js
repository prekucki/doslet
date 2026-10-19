// ABI v1 bridge. The generated module owns only small CPU/dispatch scratch;
// the Rust host keeps exclusive ownership of guest RAM and devices.
// host: {abi_ptr(), abi_len(), poll(completed, remaining)}; memory: host WASM memory.
export async function instantiateWasmCore(bytes, host, memory) {
    const module = await WebAssembly.compile(bytes);
    const abi = WebAssembly.Module.customSections(module, 'doslet.abi');
    if (abi.length !== 1 || abi[0].byteLength !== 4 || new DataView(abi[0]).getUint32(0, true) !== 1) {
        throw new Error('Unsupported Doslet WASM core ABI');
    }
    let instance, hostView, moduleView, running = false, failed = false;
    const size = host.abi_len();
    if (size !== 65600) throw new Error('Invalid Doslet WASM core scratch size');
    const refresh = () => {
        if (!hostView || hostView.buffer !== memory.buffer) {
            hostView = new Uint8Array(memory.buffer, host.abi_ptr(), size);
        }
        if (!moduleView) moduleView = new Uint8Array(instance.exports.memory.buffer, 0, size);
    };
    instance = await WebAssembly.instantiate(module, {doslet: {
        poll(completed, remaining) {
            refresh();
            hostView.set(moduleView);
            const next = host.poll(completed >>> 0, remaining >>> 0);
            refresh(); // An interpreter callback may grow the host WASM memory.
            moduleView.set(hostView);
            return next;
        }
    }});
    return {
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
