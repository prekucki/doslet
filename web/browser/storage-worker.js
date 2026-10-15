// This worker owns IndexedDB and holds each writer lock until explicitly closed.
const stores = new Map();
const MAX_BATCH = 16 * 1024 * 1024;
function fault(code, message) { return Object.assign(new Error(message), {code}); }
function transaction(db, mode) {
    try { return db.transaction(['namespaces'], mode, {durability: 'strict'}); }
    catch { return db.transaction(['namespaces'], mode); }
}
function openDb() {
    return new Promise((resolve, reject) => {
        const request = indexedDB.open('doslet-saves', 1);
        request.onupgradeneeded = () => request.result.createObjectStore('namespaces');
        request.onerror = () => reject(request.error);
        request.onblocked = () => reject(fault('BusyWriter', 'Close other Doslet tabs before upgrading save storage'));
        request.onsuccess = () => {
            const db = request.result;
            db.onversionchange = () => { db.close(); for (const store of stores.values()) store.release(); stores.clear(); };
            resolve(db);
        };
    });
}
// All work is prepared before entering IDB. Only request callbacks run inside it.
function update(db, namespace, action) {
    return new Promise((resolve, reject) => {
        const tx = transaction(db, 'readwrite');
        const table = tx.objectStore('namespaces');
        let result, failure;
        const request = table.get(namespace);
        request.onsuccess = () => {
            try { const next = action(request.result); result = next; table.put(next, namespace); }
            catch (error) { failure = error; tx.abort(); }
        };
        tx.oncomplete = () => resolve(result);
        tx.onabort = tx.onerror = () => reject(failure || tx.error || fault('Unavailable', 'Save transaction aborted'));
    });
}
async function acquire(namespace, base) {
    if (stores.has(namespace)) {
        const current = stores.get(namespace);
        if (current.base !== base) throw fault('MissingBase', 'Save namespace belongs to a different base');
        return current.state;
    }
    if (!navigator.locks) throw fault('Unavailable', 'Persistent saves require Web Locks in a secure context');
    let accept, decline;
    const acquired = new Promise((resolve, reject) => { accept = resolve; decline = reject; });
    navigator.locks.request(`doslet-save:${namespace}`, {mode: 'exclusive', ifAvailable: true}, async lock => {
        if (!lock) { decline(fault('BusyWriter', 'This save store is already open in another tab')); return; }
        let release;
        const held = new Promise(resolve => { release = resolve; });
        let db;
        try {
            db = await openDb();
            const state = await update(db, namespace, previous => {
                if (previous && (previous.schema !== 1 || previous.base !== base)) throw fault(previous.schema !== 1 ? 'UnsupportedSchema' : 'MissingBase', 'Save store schema or base does not match');
                const next = previous || {schema: 1, base, revision: '0', fence: '0', payload: new Uint8Array(), mutations: []};
                return {...next, fence: String(BigInt(next.fence) + 1n)};
            });
            // Hash outside the transaction; opening never acknowledges damaged
            // persisted bytes merely because their structural header parses.
            if(state.revision !== '0') {
                const last=state.mutations.find(item=>item.revision===state.revision);
                if(!last || checksum(new Uint8Array(state.payload))!==last.digest) throw fault('Corrupt','Stored save payload checksum mismatch');
            }
            stores.set(namespace, {db, base, release, state}); accept(state);
            await held;
        } catch (error) { decline(error); }
        finally { db?.close(); stores.delete(namespace); }
    }).catch(decline);
    return acquired;
}
function checksum(bytes) {
    let value = 0xcbf29ce484222325n;
    for (const byte of bytes) value = BigInt.asUintN(64, (value ^ BigInt(byte)) * 0x100000001b3n);
    return value.toString();
}
async function commit(message) {
    const store = stores.get(message.namespace);
    if (!store) throw fault('BusyWriter', 'Save writer is not open');
    const payload = new Uint8Array(message.payload);
    if (payload.length > MAX_BATCH) throw fault('Quota', 'Save batch exceeds 16 MiB');
    if (checksum(payload) !== message.digest) throw fault('Corrupt', 'Save batch checksum mismatch');
    const state = await update(store.db, message.namespace, previous => {
        if (!previous || previous.base !== message.base) throw fault('MissingBase', 'Save base mismatch');
        if (previous.fence !== message.fence) throw fault('BusyWriter', 'Save writer fencing token expired');
        const duplicate = previous.mutations.find(item => item.id === message.mutation);
        if (duplicate) {
            if (duplicate.digest !== message.digest || duplicate.expected !== message.expected) throw fault('MutationConflict', 'Mutation id reused with different data');
            return previous;
        }
        if (previous.revision !== message.expected) throw fault('StaleRevision', 'Save revision changed');
        const revision = String(BigInt(previous.revision) + 1n);
        // Keep the full replay ledger for this bounded overlay namespace.
        if (previous.mutations.length >= 65536) throw fault('Quota', 'Save mutation ledger full; export into a new store');
        return {...previous, revision, payload, mutations: [...previous.mutations, {id: message.mutation, digest: message.digest, expected: message.expected, revision}]};
    });
    store.state = state;
    return {revision: state.mutations.find(item => item.id === message.mutation)?.revision || state.revision};
}
const chains = new Map();
self.onmessage = ({data}) => {
    // Ordered per namespace: load/reset cannot pass an unresolved commit.
    const before = chains.get(data.namespace) || Promise.resolve();
    const work = before.catch(() => {}).then(async () => {
        let result;
        if (data.kind === 'open') result = await acquire(data.namespace, data.base);
        else if (data.kind === 'commit') result = await commit(data);
        else if (data.kind === 'close') { stores.get(data.namespace)?.release(); result = {}; }
        else throw fault('Unavailable', 'Unknown storage operation');
        self.postMessage({id: data.id, ok: true, ...result});
    }).catch(error => self.postMessage({id: data.id, ok: false, code: error.code || (error.name === 'QuotaExceededError' ? 'Quota' : 'Unavailable'), error: String(error)}));
    chains.set(data.namespace, work);
};
