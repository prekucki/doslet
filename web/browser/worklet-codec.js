// wasm-bindgen decodes Rust exception strings even in a module with numeric
// audio APIs. AudioWorkletGlobalScope does not provide TextDecoder in Firefox.
// This fallback is used during module setup and on errors, never for samples.
if (typeof globalThis.TextDecoder === 'undefined') {
    globalThis.TextDecoder = class {
        decode(bytes = new Uint8Array()) {
            let encoded = '';
            for (const byte of bytes) encoded += '%' + byte.toString(16).padStart(2, '0');
            return decodeURIComponent(encoded);
        }
    };
}
