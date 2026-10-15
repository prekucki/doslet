// Positional media adapters. Neither adapter hands the complete image to WASM.
export class MediaError extends Error {
    constructor(code, message) { super(message); this.code = code; }
}
export function hex(bytes) { return [...bytes].map(v => v.toString(16).padStart(2, '0')).join(''); }
export async function fingerprint(text) {
    return new Uint8Array(await crypto.subtle.digest('SHA-256', new TextEncoder().encode(text)));
}
async function boundedBody(response, limit, signal) {
    const reader = response.body?.getReader();
    if (!reader) throw new MediaError('Unavailable', 'Range response has no readable body');
    const output = new Uint8Array(limit);
    let used = 0;
    try {
        for (;;) {
            if (signal.aborted) throw new MediaError('Unavailable', 'Media request cancelled');
            const {done, value} = await reader.read();
            if (done) break;
            if (used + value.length > limit) throw new MediaError('Corrupt', 'Range response exceeds requested length');
            output.set(value, used); used += value.length;
        }
    } finally { await reader.cancel().catch(() => {}); reader.releaseLock(); }
    if (used !== limit) throw new MediaError('Corrupt', 'Truncated range response');
    return output;
}
export class HttpMedia {
    constructor(url) {
        this.url = new URL(url);
        if (this.url.protocol !== 'https:') throw new MediaError('Unavailable', 'Remote ISO requires HTTPS');
        this.length = 0; this.etag = null; this.finalUrl = null; this.validator = null;
        this.requests = new Set(); this.closed = false;
    }
    async prepare() {
        await this.request(0, 65536, true);
        this.validator = await fingerprint(`${this.finalUrl}\n${this.etag}\n${this.length}`);
        return this;
    }
    async read(offset, length) { return this.request(offset, length, false); }
    async request(offset, length, probe) {
        if (this.closed) throw new MediaError('Unavailable', 'Media source is closed');
        if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || offset < 0 || length < 1 || length > 131072 || (!probe && offset + length > this.length))
            throw new MediaError('OutOfBounds', 'Invalid ISO byte range');
        if (this.requests.size >= 4) throw new MediaError('Unavailable', 'Four ISO reads already in flight');
        const controller = new AbortController(); this.requests.add(controller);
        const deadline = performance.now() + 35000;
        try {
            for (let attempt = 0; attempt < 3; attempt++) {
                if (attempt) await new Promise(resolve => setTimeout(resolve, attempt === 1 ? 250 : 1000));
                if (controller.signal.aborted || performance.now() >= deadline) throw new MediaError('Unavailable', 'ISO read deadline exceeded');
                const perAttempt = new AbortController();
                const cancel = () => perAttempt.abort();
                controller.signal.addEventListener('abort', cancel, {once: true});
                const timer = setTimeout(cancel, Math.min(10000, deadline - performance.now()));
                try {
                    const headers = {Range: `bytes=${offset}-${offset + length - 1}`};
                    if (!probe) headers['If-Match'] = this.etag;
                    const response = await fetch(this.url, {headers, mode: 'cors', credentials: 'omit', cache: 'no-store', signal: perAttempt.signal});
                    if (response.status >= 500 && attempt < 2) { await response.body?.cancel(); continue; }
                    if (response.status === 412) throw new MediaError('IdentityChanged', 'ISO changed on the server; remount it');
                    if (response.status !== 206) {
                        await response.body?.cancel();
                        throw new MediaError(response.status === 416 ? 'OutOfBounds' : 'UnsupportedRange', `ISO server returned HTTP ${response.status}; a 206 byte range response is required`);
                    }
                    if (new URL(response.url).protocol !== 'https:') throw new MediaError('Unavailable', 'ISO redirect must remain HTTPS');
                    const range = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers.get('Content-Range') || '');
                    const etag = response.headers.get('ETag');
                    const encoding = response.headers.get('Content-Encoding');
                    if (!range || !etag || !/^"[^"\r\n]*"$/.test(etag)) throw new MediaError('UnsupportedRange', 'ISO requires exposed Content-Range and a strong ETag (check CORS)');
                    if (encoding && encoding !== 'identity') throw new MediaError('UnsupportedRange', 'Compressed ISO range responses are unsupported');
                    const [start, end, total] = range.slice(1).map(Number);
                    if (![start, end, total].every(Number.isSafeInteger) || total < 32768 || start !== offset || end !== Math.min(offset + length, total) - 1)
                        throw new MediaError('Corrupt', 'ISO server returned an inconsistent Content-Range');
                    if (!probe && (total !== this.length || etag !== this.etag || response.url !== this.finalUrl))
                        throw new MediaError('IdentityChanged', 'ISO identity changed during range reads');
                    const bytes = await boundedBody(response, end - start + 1, perAttempt.signal);
                    if (probe) { this.length = total; this.etag = etag; this.finalUrl = response.url; }
                    return bytes;
                } catch (error) {
                    if (error instanceof MediaError || attempt === 2 || controller.signal.aborted) throw error;
                } finally { clearTimeout(timer); controller.signal.removeEventListener('abort', cancel); perAttempt.abort(); }
            }
        } finally { this.requests.delete(controller); }
        throw new MediaError('Unavailable', 'ISO retries exhausted');
    }
    close() { this.closed = true; for (const request of this.requests) request.abort(); }
}
export class BlobMedia {
    constructor(blob) { this.blob = blob; this.length = blob.size; this.closed = false; }
    async prepare() {
        if (this.length < 32768 || !Number.isSafeInteger(this.length)) throw new MediaError('Corrupt', 'Invalid ISO size');
        // A Blob is immutable for this attachment. This identity is scoped to it.
        this.validator = crypto.getRandomValues(new Uint8Array(32));
        return this;
    }
    async read(offset, length) {
        if (this.closed || offset < 0 || length > 131072 || offset + length > this.length) throw new MediaError('OutOfBounds', 'Invalid Blob range');
        const bytes = new Uint8Array(await this.blob.slice(offset, offset + length).arrayBuffer());
        if (bytes.length !== length) throw new MediaError('Corrupt', 'Truncated local ISO');
        return bytes;
    }
    close() { this.closed = true; }
}
