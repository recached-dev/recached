// Stands in for the `recached-edge` package during tests.
//
// It is a *peer* dependency, so it is not installed here — but `context.tsx`
// imports `createCache` from it at module scope, which has to resolve for the
// module to load at all. `vitest.config.ts` aliases the specifier to this file.
//
// `setCreateCache` lets a test decide what the provider gets, including a
// promise it controls, which is how the "renders nothing until ready" case is
// tested without timing guesswork.

type CreateCache = (options?: unknown) => Promise<unknown>;

let impl: CreateCache = async () => ({});

export function setCreateCache(fn: CreateCache): void {
  impl = fn;
}

export function resetCreateCache(): void {
  impl = async () => ({});
}

export const createCache: CreateCache = (options) => impl(options);

/** Type-only in the source; present so `import { Cache }` resolves at runtime. */
export class Cache {}
