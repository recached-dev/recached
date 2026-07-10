import { useSyncExternalStore } from 'react';
import { useRecached } from './context';

/**
 * Reactively read a string key from the Recached store.
 *
 * The component re-renders automatically whenever the key is written or
 * deleted — whether the mutation originated locally, from another tab via
 * BroadcastChannel, or from another client via the server WebSocket.
 *
 * Returns `null` when the key does not exist or has expired.
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
    () => cache.get(key),
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
