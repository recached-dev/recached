// ── Types ─────────────────────────────────────────────────────────────────────
let _module = null;
let _initPromise = null;
async function ensureModule() {
    if (_module)
        return _module;
    if (!_initPromise) {
        _initPromise = (async () => {
            const mod = await import('./pkg/recached_edge.js');
            await mod.default(); // initialise WASM — idempotent on repeated calls
            _module = mod;
            return mod;
        })();
    }
    return _initPromise;
}
// ── Cache ─────────────────────────────────────────────────────────────────────
/**
 * A typed wrapper around the Recached WASM cache.
 *
 * Obtain an instance via {@link createCache} rather than `new Cache()`.
 *
 * ```ts
 * const cache = await createCache({ persistence: true });
 * cache.set('theme', 'dark');
 * cache.get('theme'); // "dark"
 * ```
 */
export class Cache {
    /** @internal */
    constructor(raw) {
        this._mutationListeners = new Set();
        this._messageListeners = new Map();
        /** @internal Arrow function so `this` is always bound when passed as a callback. */
        this._notifyMutation = () => {
            for (const cb of this._mutationListeners)
                cb();
        };
        /** @internal */
        this._notifyMessage = (channel, message) => {
            const listeners = this._messageListeners.get(channel);
            if (listeners) {
                for (const cb of listeners)
                    cb(message);
            }
        };
        /** @internal Ref-counts per live-query pattern so components sharing a
         * pattern don't cancel each other's server subscription. */
        this._liveQueryRefs = new Map();
        this.raw = raw;
        raw.set_mutation_callback(this._notifyMutation);
        raw.set_message_callback(this._notifyMessage);
    }
    /**
     * Subscribe to store mutations from any source — local writes, server
     * WebSocket push, and BroadcastChannel cross-tab sync.
     *
     * Returns an unsubscribe function. Pass directly to React's
     * `useSyncExternalStore` `subscribe` parameter.
     *
     * ```ts
     * useSyncExternalStore(
     *   cache.onMutation.bind(cache),
     *   () => cache.get('key'),
     *   () => null,
     * );
     * ```
     */
    onMutation(cb) {
        this._mutationListeners.add(cb);
        return () => this._mutationListeners.delete(cb);
    }
    /**
     * Subscribe to pub/sub messages on `channel`.
     *
     * Returns an unsubscribe function. Call it to stop receiving messages.
     * Does not send `UNSUBSCRIBE` to the server — use {@link unsubscribe} for that.
     *
     * ```ts
     * cache.subscribe('notifications');
     * const stop = cache.onMessage('notifications', (msg) => console.log(msg));
     * // later:
     * stop();
     * cache.unsubscribe('notifications');
     * ```
     */
    onMessage(channel, cb) {
        let listeners = this._messageListeners.get(channel);
        if (!listeners) {
            listeners = new Set();
            this._messageListeners.set(channel, listeners);
        }
        listeners.add(cb);
        return () => {
            listeners.delete(cb);
            if (listeners.size === 0)
                this._messageListeners.delete(channel);
        };
    }
    // ── Reads ─────────────────────────────────────────────────────────────────
    /**
     * Return the value for `key`, or `null` if the key does not exist or has expired.
     *
     * Always served from local WASM memory — zero network latency.
     */
    get(key) {
        return this.raw.get(key) ?? null;
    }
    /**
     * Return a JSON-parsed value stored under `key`, or `null` if the key is
     * missing, expired, or not valid JSON.
     *
     * ```ts
     * interface User { id: number; name: string }
     * const user = cache.getJSON<User>('user:42'); // User | null
     * ```
     */
    getJSON(key) {
        const raw = this.get(key);
        if (raw === null)
            return null;
        try {
            return JSON.parse(raw);
        }
        catch {
            return null;
        }
    }
    /** Return `true` if `key` exists and has not expired. */
    exists(key) {
        return this.raw.exists(key);
    }
    /**
     * Return the remaining TTL in seconds.
     * - `-1` — key exists with no expiry
     * - `-2` — key does not exist
     */
    ttl(key) {
        return this.raw.ttl(key);
    }
    // ── Writes ────────────────────────────────────────────────────────────────
    /**
     * Store a string value. Syncs to the server and other tabs when connected.
     */
    set(key, value) {
        // raw.set fires the mutation callback registered in the constructor, so
        // listeners are already notified — no second _notifyMutation() here.
        this.raw.set(key, value);
    }
    /**
     * Store a string value with a TTL (seconds). The key is deleted automatically
     * once the TTL elapses.
     */
    setEx(key, value, seconds) {
        this.raw.set_ex(key, value, seconds);
    }
    /**
     * Serialize `value` as JSON and store it under `key`.
     * Pass `ttl` (seconds) to have the key expire automatically.
     *
     * ```ts
     * cache.setJSON('user:42', { id: 42, name: 'Alice' }, 300); // expires in 5 min
     * ```
     */
    setJSON(key, value, ttl) {
        const serialized = JSON.stringify(value);
        if (ttl !== undefined) {
            this.raw.set_ex(key, serialized, ttl);
        }
        else {
            this.raw.set(key, serialized);
        }
    }
    /**
     * Delete `key`.
     *
     * Returns `true` if the key existed, `false` if it did not.
     * Syncs to the server and other tabs when connected.
     */
    del(key) {
        const existed = this.raw.del(key) === 1;
        return existed;
    }
    // ── Pub/sub ───────────────────────────────────────────────────────────────
    /**
     * Subscribe to a server pub/sub channel. Push messages arrive via the
     * WebSocket `onmessage` callback.
     */
    subscribe(channel) {
        this.raw.subscribe(channel);
    }
    /** Unsubscribe from a server pub/sub channel. */
    unsubscribe(channel) {
        this.raw.unsubscribe(channel);
    }
    /**
     * Publish a message to a server pub/sub channel. All subscribers — browser
     * and server-side — receive the message.
     */
    publish(channel, message) {
        this.raw.publish(channel, message);
    }
    // ── Sync scoping & live queries ───────────────────────────────────────────
    /**
     * Present a signed sync-scope token (strict servers — `RECACHED_SYNC_SECRET`).
     * Mint it on your backend and hand it to the page; see the Sync Scopes docs.
     */
    syncToken(token) {
        this.raw.sync_token(token);
    }
    /**
     * Scope this connection's sync to glob patterns (servers without a sync
     * secret only). A bandwidth filter, not an authorization boundary.
     */
    syncScopes(patterns) {
        this.raw.sync_scopes(patterns.join(','));
    }
    /**
     * Start a live query: the server sends the current state of every key
     * matching `pattern` (merged into the local store), then streams every
     * change to matching keys — including keys created later.
     *
     * Returns a stop function. Calls are ref-counted per pattern: the server
     * subscription ends when the last caller stops.
     *
     * ```ts
     * const stop = cache.liveQuery('cart:42:*');
     * cache.onMutation(() => {
     *   render(cache.getMatching('cart:42:*'));
     * });
     * // later:
     * stop();
     * ```
     */
    liveQuery(pattern) {
        const refs = this._liveQueryRefs.get(pattern) ?? 0;
        this._liveQueryRefs.set(pattern, refs + 1);
        if (refs === 0) {
            this.raw.live_query(pattern);
        }
        let stopped = false;
        return () => {
            if (stopped)
                return;
            stopped = true;
            const now = (this._liveQueryRefs.get(pattern) ?? 1) - 1;
            if (now <= 0) {
                this._liveQueryRefs.delete(pattern);
                this.raw.live_unquery(pattern);
            }
            else {
                this._liveQueryRefs.set(pattern, now);
            }
        };
    }
    /**
     * Snapshot of local keys matching a glob pattern, as `[key, value]` pairs.
     * Values are strings; keys holding collection types come back as `null`
     * (read those with typed accessors).
     *
     * Served entirely from local WASM memory — zero network latency.
     */
    getMatching(pattern) {
        return this.raw.get_matching(pattern);
    }
    // ── Persistence ───────────────────────────────────────────────────────────
    /**
     * Erase the IndexedDB WAL. The in-memory store is not affected.
     *
     * Use on sign-out so the next session starts with an empty cache.
     */
    clearPersistence() {
        return this.raw.clear_persistence();
    }
}
// ── Public API ────────────────────────────────────────────────────────────────
/**
 * Initialise the WASM module eagerly.
 *
 * `createCache` calls this automatically on first use, so calling `init()`
 * directly is only necessary when you want to front-load the WASM download
 * (e.g. during a loading screen).
 *
 * ```ts
 * import { init, createCache } from 'recached-edge';
 *
 * await init(); // start downloading WASM now
 * // ... other app setup ...
 * const cache = await createCache(); // resolves immediately — already loaded
 * ```
 */
export async function init() {
    await ensureModule();
}
/**
 * Create a {@link Cache} instance.
 *
 * Loads and initialises the WASM module on first call (subsequent calls reuse
 * the existing module). Options are applied in order:
 * `persistence` → `broadcastChannel` → `connect` + `auth`.
 *
 * ```ts
 * import { createCache } from 'recached-edge';
 *
 * // Local-only, survives refresh
 * const cache = await createCache({ persistence: true });
 *
 * // With server sync
 * const cache = await createCache({
 *   persistence: true,
 *   connect: { url: 'ws://localhost:6380', password: 'secret' },
 * });
 *
 * cache.set('theme', 'dark');
 * cache.getJSON<User>('user:42'); // User | null
 *
 * // --- page refresh ---
 * const cache = await createCache({ persistence: true });
 * cache.get('theme'); // "dark" — restored from IndexedDB, zero network
 * ```
 */
export async function createCache(options = {}) {
    const mod = await ensureModule();
    const raw = new mod.RecachedCache();
    if (options.persistence) {
        await raw.enable_persistence();
    }
    if (options.broadcastChannel) {
        raw.broadcast(options.broadcastChannel);
    }
    if (options.connect) {
        raw.connect(options.connect.url);
        if (options.connect.password) {
            raw.auth(options.connect.password);
        }
        if (options.connect.syncToken) {
            raw.sync_token(options.connect.syncToken);
        }
        else if (options.connect.syncScopes?.length) {
            raw.sync_scopes(options.connect.syncScopes.join(','));
        }
    }
    return new Cache(raw);
}
