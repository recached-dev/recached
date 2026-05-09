// ── Types ─────────────────────────────────────────────────────────────────────

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

// ── Internal wasm-bindgen shape ───────────────────────────────────────────────
// Structural type — satisfied by the generated RecachedCache class.

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

// ── WASM module singleton ─────────────────────────────────────────────────────

type WasmModule = typeof import('./pkg/recached_edge.js');

let _module: WasmModule | null = null;
let _initPromise: Promise<WasmModule> | null = null;

async function ensureModule(): Promise<WasmModule> {
  if (_module) return _module;
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
  readonly raw: RawCache;

  private readonly _mutationListeners = new Set<() => void>();

  /** @internal Arrow function so `this` is always bound when passed as a callback. */
  private readonly _notifyMutation = (): void => {
    for (const cb of this._mutationListeners) cb();
  };

  /** @internal */
  constructor(raw: RawCache) {
    this.raw = raw;
    raw.set_mutation_callback(this._notifyMutation);
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
  onMutation(cb: () => void): () => void {
    this._mutationListeners.add(cb);
    return () => this._mutationListeners.delete(cb);
  }

  // ── Reads ─────────────────────────────────────────────────────────────────

  /**
   * Return the value for `key`, or `null` if the key does not exist or has expired.
   *
   * Always served from local WASM memory — zero network latency.
   */
  get(key: string): string | null {
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
  getJSON<T>(key: string): T | null {
    const raw = this.get(key);
    if (raw === null) return null;
    try {
      return JSON.parse(raw) as T;
    } catch {
      return null;
    }
  }

  /** Return `true` if `key` exists and has not expired. */
  exists(key: string): boolean {
    return this.raw.exists(key);
  }

  /**
   * Return the remaining TTL in seconds.
   * - `-1` — key exists with no expiry
   * - `-2` — key does not exist
   */
  ttl(key: string): number {
    return this.raw.ttl(key);
  }

  // ── Writes ────────────────────────────────────────────────────────────────

  /**
   * Store a string value. Syncs to the server and other tabs when connected.
   */
  set(key: string, value: string): void {
    this.raw.set(key, value);
    this._notifyMutation();
  }

  /**
   * Store a string value with a TTL (seconds). The key is deleted automatically
   * once the TTL elapses.
   */
  setEx(key: string, value: string, seconds: number): void {
    this.raw.set_ex(key, value, seconds);
    this._notifyMutation();
  }

  /**
   * Serialize `value` as JSON and store it under `key`.
   * Pass `ttl` (seconds) to have the key expire automatically.
   *
   * ```ts
   * cache.setJSON('user:42', { id: 42, name: 'Alice' }, 300); // expires in 5 min
   * ```
   */
  setJSON<T>(key: string, value: T, ttl?: number): void {
    const serialized = JSON.stringify(value);
    if (ttl !== undefined) {
      this.raw.set_ex(key, serialized, ttl);
    } else {
      this.raw.set(key, serialized);
    }
    this._notifyMutation();
  }

  /**
   * Delete `key`.
   *
   * Returns `true` if the key existed, `false` if it did not.
   * Syncs to the server and other tabs when connected.
   */
  del(key: string): boolean {
    const existed = this.raw.del(key) === 1;
    this._notifyMutation();
    return existed;
  }

  // ── Pub/sub ───────────────────────────────────────────────────────────────

  /**
   * Subscribe to a server pub/sub channel. Push messages arrive via the
   * WebSocket `onmessage` callback.
   */
  subscribe(channel: string): void {
    this.raw.subscribe(channel);
  }

  /** Unsubscribe from a server pub/sub channel. */
  unsubscribe(channel: string): void {
    this.raw.unsubscribe(channel);
  }

  /**
   * Publish a message to a server pub/sub channel. All subscribers — browser
   * and server-side — receive the message.
   */
  publish(channel: string, message: string): void {
    this.raw.publish(channel, message);
  }

  // ── Persistence ───────────────────────────────────────────────────────────

  /**
   * Erase the IndexedDB WAL. The in-memory store is not affected.
   *
   * Use on sign-out so the next session starts with an empty cache.
   */
  clearPersistence(): Promise<void> {
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
export async function init(): Promise<void> {
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
export async function createCache(options: CacheOptions = {}): Promise<Cache> {
  const mod = await ensureModule();
  const raw = new mod.RecachedCache() as unknown as RawCache;

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
  }

  return new Cache(raw);
}
