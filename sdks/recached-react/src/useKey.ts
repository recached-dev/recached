import { useSyncExternalStore } from 'react';
import { useRecached } from './context';

/**
 * Reactively read a string key from the Recached store.
 *
 * The component re-renders automatically whenever the key is written or
 * deleted — whether the mutation originated locally, from another tab via
 * BroadcastChannel, or from another client via the server WebSocket.
 *
 * Returns `null` when the key does not exist, has expired, or holds a value that
 * is not valid UTF-8 — a binary value has no string form, so read it with
 * {@link useKeyBytes} instead.
 *
 * Built on React 18's `useSyncExternalStore` — safe with concurrent features.
 *
 * ```tsx
 * function ThemeButton() {
 *   const cache = useRecached();
 *   const theme = useKey('theme');
 *   return (
 *     <button onClick={() => cache.set('theme', theme === 'dark' ? 'light' : 'dark')}>
 *       {theme ?? 'light'}
 *     </button>
 *   );
 * }
 * ```
 */
export function useKey(key: string): string | null {
  const cache = useRecached();
  return useSyncExternalStore(
    (cb) => cache.onMutation(cb),
    // `get` throws on a binary value. This runs as a `getSnapshot`, so letting
    // it propagate would take down the render tree over a value the component
    // simply cannot display — report it as absent and let `useKeyBytes` read it.
    () => {
      try {
        return cache.get(key);
      } catch {
        return null;
      }
    },
    () => null,
  );
}

/**
 * Reactively read a key as raw bytes.
 *
 * Behaves identically to {@link useKey} but returns the value's bytes, so it
 * works for binary values a backend wrote — compressed payloads, protobuf,
 * images. Text values come back as their UTF-8 bytes.
 *
 * ```tsx
 * function Thumbnail() {
 *   const bytes = useKeyBytes('thumb:42');
 *   const src = useMemo(
 *     () => (bytes ? URL.createObjectURL(new Blob([bytes])) : null),
 *     [bytes],
 *   );
 *   return src ? <img src={src} /> : null;
 * }
 * ```
 */
export function useKeyBytes(key: string): Uint8Array | null {
  const cache = useRecached();
  return useSyncExternalStore(
    (cb) => cache.onMutation(cb),
    () => cache.getBytes(key),
    () => null,
  );
}

/**
 * Reactively read a JSON-parsed value from the Recached store.
 *
 * Behaves identically to {@link useKey} but parses the stored string as JSON.
 * Returns `null` when the key is missing, expired, or contains invalid JSON.
 *
 * ```tsx
 * interface User { id: number; name: string }
 *
 * function UserCard() {
 *   const user = useKeyJSON<User>('user:42');
 *   if (!user) return <Spinner />;
 *   return <p>{user.name}</p>;
 * }
 * ```
 */
export function useKeyJSON<T>(key: string): T | null {
  const cache = useRecached();
  return useSyncExternalStore(
    (cb) => cache.onMutation(cb),
    () => cache.getJSON<T>(key),
    () => null,
  );
}
