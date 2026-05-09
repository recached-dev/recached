import { inject, type App, type InjectionKey } from 'vue';
import { createCache, type Cache, type CacheOptions } from 'recached-edge';

const CACHE_KEY: InjectionKey<Cache> = Symbol('recached');

// Re-exported so callers can provide a pre-built cache via app.provide(CACHE_KEY, cache).
export { CACHE_KEY };

/**
 * Vue plugin that creates a Recached cache and provides it to the entire app.
 *
 * ```ts
 * import { createApp } from 'vue'
 * import { RecachedPlugin } from '@recached/vue'
 *
 * const app = createApp(App)
 * app.use(RecachedPlugin, {
 *   persistence: true,
 *   connect: { url: 'ws://localhost:6380' },
 * })
 * app.mount('#app')
 * ```
 */
export const RecachedPlugin = {
  async install(app: App, options?: CacheOptions) {
    const cache = await createCache(options);
    app.provide(CACHE_KEY, cache);
  },
};

/**
 * Return the `Cache` instance provided by `RecachedPlugin`.
 *
 * Use this when you need to call `set`, `del`, `publish`, etc. imperatively.
 * Throws if called before the plugin has resolved (i.e. before `app.mount`).
 *
 * ```ts
 * const cache = useRecached()
 * cache.set('theme', 'dark')
 * ```
 */
export function useRecached(): Cache {
  const cache = inject<Cache>(CACHE_KEY);
  if (!cache) {
    throw new Error(
      'useRecached(): no cache found. Make sure RecachedPlugin is installed before mounting the app.',
    );
  }
  return cache;
}
