import { vi } from 'vitest';

/**
 * A stand-in for `Cache` from `recached-edge`.
 *
 * The hooks only ever touch the SDK through this surface, so faking it keeps
 * the tests about React behaviour — subscription lifecycle, snapshot identity,
 * unmount cleanup — rather than about the engine. `emit()` plays the role the
 * wasm module plays in production: something changed, re-read.
 */
export function makeFakeCache() {
  const store = new Map<string, string>();
  const mutationListeners = new Set<() => void>();
  const messageListeners = new Map<string, Set<(m: string | Uint8Array) => void>>();

  return {
    /** Fire the mutation callback the way a local write or server push would. */
    emit() {
      for (const cb of [...mutationListeners]) cb();
    },
    /** Deliver a pub/sub message to the channel's listeners. */
    deliver(channel: string, msg: string | Uint8Array) {
      for (const cb of [...(messageListeners.get(channel) ?? [])]) cb(msg);
    },
    /** Write without notifying, so a test can control exactly when re-reads happen. */
    seed(key: string, value: string) {
      store.set(key, value);
    },

    onMutation: vi.fn((cb: () => void) => {
      mutationListeners.add(cb);
      return () => mutationListeners.delete(cb);
    }),
    onMessage: vi.fn((channel: string, cb: (m: string | Uint8Array) => void) => {
      let set = messageListeners.get(channel);
      if (!set) messageListeners.set(channel, (set = new Set()));
      set.add(cb);
      return () => set!.delete(cb);
    }),

    get: vi.fn((key: string) => store.get(key) ?? null),
    // Faithful to the real binding: every call crosses the wasm boundary and
    // allocates a *fresh* Uint8Array, so the identity is never stable. A fake
    // returning one shared instance would hide snapshot-identity bugs.
    getBytes: vi.fn((key: string): Uint8Array | null => {
      const raw = store.get(key);
      return raw === undefined ? null : new TextEncoder().encode(raw);
    }),
    getJSON: vi.fn(<T,>(key: string): T | null => {
      const raw = store.get(key);
      if (raw === undefined) return null;
      try {
        return JSON.parse(raw) as T;
      } catch {
        return null;
      }
    }),
    set: vi.fn((key: string, value: string) => {
      store.set(key, value);
    }),
    getMatching: vi.fn((): Array<[string, string | Uint8Array | null]> => []),
    liveQuery: vi.fn(() => vi.fn()),
    subscribe: vi.fn(),
    unsubscribe: vi.fn(),

    /** Number of mutation listeners still registered — leak detection. */
    get listenerCount() {
      return mutationListeners.size;
    },
  };
}

export type FakeCache = ReturnType<typeof makeFakeCache>;
