// Compile-time proof that the generated wasm-bindgen class still satisfies the
// `RawCache` interface `sdk.ts` casts to.
//
// `createCache` loads the module dynamically and casts the instance through
// `unknown`, which silences every type error at that boundary. So `RawCache` is
// an assertion nothing verified: when Rust's `incr_by(delta: i64)` made
// wasm-bindgen emit `bigint` and the interface still said `number`, `tsc` was
// happy and `cache.incr()` threw `Cannot convert 1 to a BigInt` on every call,
// in every runtime, for several releases.
//
// This file is deliberately NOT in tsconfig.json's `include` (which is only
// `sdk.ts`, so nothing here is emitted or published). It is checked by
// `npm run typecheck:bindings` via tsconfig.bindings.json, which requires a
// real `wasm-pack` build — the stub `.d.ts` that CI's typecheck job generates
// cannot answer this question.

import type { RawCache } from './sdk.js';
import type { RecachedCache } from './pkg/recached_edge.js';

/** Fails to compile unless `T` structurally satisfies {@link RawCache}. */
type MustSatisfyRawCache<T extends RawCache> = T;

// The assertion. If wasm-bindgen changes a signature — a new numeric width, a
// renamed method, a dropped one — this stops compiling and names the member.
export type _GeneratedBindingConforms = MustSatisfyRawCache<RecachedCache>;
