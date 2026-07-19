import { useEffect, useRef, useSyncExternalStore } from 'react';
import { useRecached } from './context';

/** A key/value pair from the local store. Collection-typed keys have `null`
 * values — read those with typed accessors. A value that is not valid UTF-8
 * arrives as a `Uint8Array` rather than a mangled string. */
export type KeyValuePair = [key: string, value: string | Uint8Array | null];

const EMPTY: KeyValuePair[] = [];

/**
 * Live query: reactively read every key matching a glob pattern.
 *
 * On mount, the server sends the current state of all matching keys (merged
 * into the local store) and then streams every change to matching keys —
 * including keys created after subscribing. The component re-renders on each
 * change, always reading from local WASM memory.
 *
 * Pairs are sorted by key. The server subscription is ref-counted, so any
 * number of components can share a pattern; it ends when the last one
 * unmounts.
 *
 * Under strict sync scoping the pattern must sit inside the connection's
 * granted scopes.
 *
 * ```tsx
 * function CartList() {
 *   const items = useKeys('cart:42:item:*');
 *   return (
 *     <ul>
 *       {items.map(([key, qty]) => (
 *         <li key={key}>{key.split(':').pop()}: {qty}</li>
 *       ))}
 *     </ul>
 *   );
 * }
 * ```
 */
export function useKeys(pattern: string): KeyValuePair[] {
  const cache = useRecached();

  // liveQuery returns its stop function — exactly the shape useEffect wants.
  useEffect(() => cache.liveQuery(pattern), [cache, pattern]);

  // useSyncExternalStore compares snapshots with Object.is, so getSnapshot
  // must return the *same* array until the contents actually change.
  const memo = useRef<{ pattern: string; json: string; value: KeyValuePair[] }>(undefined);
  return useSyncExternalStore(
    (cb) => cache.onMutation(cb),
    () => {
      const next = cache.getMatching(pattern);
      const json = JSON.stringify(next);
      const prev = memo.current;
      if (prev && prev.pattern === pattern && prev.json === json) return prev.value;
      memo.current = { pattern, json, value: next };
      return next;
    },
    () => EMPTY,
  );
}
