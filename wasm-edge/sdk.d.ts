export interface ConnectOptions {
    /** WebSocket URL, e.g. `"ws://localhost:6380"` or `"wss://cache.example.com"`. */
    url: string;
    /**
     * Server password. Required when the server has `RECACHED_PASSWORD` set.
     * Sent immediately after the socket opens via `AUTH`.
     */
    password?: string;
}
export interface CacheOptions {
    /**
     * Load the IndexedDB WAL and enable write-through persistence so the cache
     * survives page refreshes.
     *
     * Gracefully ignored if IndexedDB is unavailable (e.g. some private-browsing
     * modes or non-browser environments).
     *
     * @default false
     */
    persistence?: boolean;
    /**
     * BroadcastChannel name for cross-tab mutation sharing.
     * All tabs that open with the same name see each other's writes automatically,
     * with no server connection required.
     */
    broadcastChannel?: string;
    /**
     * Connect to a Recached server immediately after the cache is created.
     * Once connected, writes are pushed to the server and server-side mutations
     * are pushed down to the local WASM store automatically.
     */
    connect?: ConnectOptions;
}
interface RawCache {
    enable_persistence(): Promise<void>;
    clear_persistence(): Promise<void>;
    broadcast(channel_name: string): void;
    connect(url: string): void;
    auth(password: string): string;
    set(key: string, value: string): string;
    set_ex(key: string, value: string, seconds: number): string;
    get(key: string): string | undefined;
    del(key: string): number;
    ttl(key: string): number;
    exists(key: string): boolean;
    publish(channel: string, message: string): void;
    subscribe(channel: string): void;
    unsubscribe(channel: string): void;
    set_mutation_callback(cb: () => void): void;
    free(): void;
}
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
export declare class Cache {
    /** @internal */
    readonly raw: RawCache;
    private readonly _mutationListeners;
    /** @internal Arrow function so `this` is always bound when passed as a callback. */
    private readonly _notifyMutation;
    /** @internal */
    constructor(raw: RawCache);
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
    onMutation(cb: () => void): () => void;
    /**
     * Return the value for `key`, or `null` if the key does not exist or has expired.
     *
     * Always served from local WASM memory — zero network latency.
     */
    get(key: string): string | null;
    /**
     * Return a JSON-parsed value stored under `key`, or `null` if the key is
     * missing, expired, or not valid JSON.
     *
     * ```ts
     * interface User { id: number; name: string }
     * const user = cache.getJSON<User>('user:42'); // User | null
     * ```
     */
    getJSON<T>(key: string): T | null;
    /** Return `true` if `key` exists and has not expired. */
    exists(key: string): boolean;
    /**
     * Return the remaining TTL in seconds.
     * - `-1` — key exists with no expiry
     * - `-2` — key does not exist
     */
    ttl(key: string): number;
    /**
     * Store a string value. Syncs to the server and other tabs when connected.
     */
    set(key: string, value: string): void;
    /**
     * Store a string value with a TTL (seconds). The key is deleted automatically
     * once the TTL elapses.
     */
    setEx(key: string, value: string, seconds: number): void;
    /**
     * Serialize `value` as JSON and store it under `key`.
     * Pass `ttl` (seconds) to have the key expire automatically.
     *
     * ```ts
     * cache.setJSON('user:42', { id: 42, name: 'Alice' }, 300); // expires in 5 min
     * ```
     */
    setJSON<T>(key: string, value: T, ttl?: number): void;
    /**
     * Delete `key`.
     *
     * Returns `true` if the key existed, `false` if it did not.
     * Syncs to the server and other tabs when connected.
     */
    del(key: string): boolean;
    /**
     * Subscribe to a server pub/sub channel. Push messages arrive via the
     * WebSocket `onmessage` callback.
     */
    subscribe(channel: string): void;
    /** Unsubscribe from a server pub/sub channel. */
    unsubscribe(channel: string): void;
    /**
     * Publish a message to a server pub/sub channel. All subscribers — browser
     * and server-side — receive the message.
     */
    publish(channel: string, message: string): void;
    /**
     * Erase the IndexedDB WAL. The in-memory store is not affected.
     *
     * Use on sign-out so the next session starts with an empty cache.
     */
    clearPersistence(): Promise<void>;
}
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
export declare function init(): Promise<void>;
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
export declare function createCache(options?: CacheOptions): Promise<Cache>;
export {};
//# sourceMappingURL=sdk.d.ts.map