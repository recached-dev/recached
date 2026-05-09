import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from 'react';
import { createCache, type Cache, type CacheOptions } from 'recached-edge';

const RecachedCtx = createContext<Cache | null>(null);

interface ProviderProps {
  /**
   * Options passed to `createCache`. Ignored when `cache` is provided.
   */
  options?: CacheOptions;
  /**
   * Pass a pre-built `Cache` instance (e.g. one you created outside React).
   * When provided, `options` is ignored and no lifecycle management is done.
   */
  cache?: Cache;
  children: ReactNode;
}

/**
 * Mount this once near the root of your app. All `useKey`, `useKeyJSON`, and
 * `usePubSub` calls inside must be descendants of this provider.
 *
 * ```tsx
 * <RecachedProvider options={{ persistence: true, connect: { url: 'ws://localhost:6380' } }}>
 *   <App />
 * </RecachedProvider>
 * ```
 */
export function RecachedProvider({ options, cache: prebuilt, children }: ProviderProps) {
  const [cache, setCache] = useState<Cache | null>(prebuilt ?? null);

  useEffect(() => {
    if (prebuilt) return;
    let cancelled = false;
    createCache(options).then((c) => {
      if (!cancelled) setCache(c);
    });
    return () => {
      cancelled = true;
    };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  if (!cache) return null;
  return <RecachedCtx.Provider value={cache}>{children}</RecachedCtx.Provider>;
}

/**
 * Return the raw `Cache` instance from the nearest `<RecachedProvider>`.
 * Use this when you need to call `set`, `del`, `publish`, etc. imperatively.
 *
 * Throws if called outside a `<RecachedProvider>`.
 */
export function useRecached(): Cache {
  const cache = useContext(RecachedCtx);
  if (!cache) {
    throw new Error('useRecached must be called inside a <RecachedProvider>.');
  }
  return cache;
}
