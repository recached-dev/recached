// Unit tests for the React bindings.
//
// The engine is not under test here — a fake `Cache` stands in for it. What is
// under test is the React contract: that a mutation re-renders, that snapshots
// keep identity so `useSyncExternalStore` does not loop forever, that live
// queries and pub/sub subscriptions are released on unmount, and that the
// provider does not hand out a cache before it exists.

import { act, cleanup, render, renderHook, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ReactNode } from 'react';

import { RecachedProvider, useRecached } from '../src/context';
import { useKey, useKeyBytes, useKeyJSON } from '../src/useKey';
import { useKeys } from '../src/useKeys';
import { usePubSub } from '../src/usePubSub';
import { makeFakeCache, type FakeCache } from './fake-cache';
import { resetCreateCache, setCreateCache } from './recached-edge.stub';

let cache: FakeCache;

/** Wrap a hook in a provider holding the fake cache. */
const wrapper = ({ children }: { children: ReactNode }) => (
  <RecachedProvider cache={cache as never}>{children}</RecachedProvider>
);

beforeEach(() => {
  cache = makeFakeCache();
});

afterEach(() => {
  cleanup();
  resetCreateCache();
});

describe('RecachedProvider', () => {
  it('uses a pre-built cache immediately, without calling createCache', () => {
    const createCache = vi.fn();
    setCreateCache(createCache);
    const { result } = renderHook(() => useRecached(), { wrapper });
    expect(result.current).toBe(cache);
    expect(createCache).not.toHaveBeenCalled();
  });

  it('renders nothing until the cache resolves, then renders children', async () => {
    let resolve!: (c: unknown) => void;
    setCreateCache(() => new Promise((r) => (resolve = r)));

    render(
      <RecachedProvider options={{ persistence: true }}>
        <span>ready</span>
      </RecachedProvider>,
    );
    // The effect has run but createCache has not settled: children are absent,
    // which is why wrapping a whole app opts the page out of SSR.
    expect(screen.queryByText('ready')).toBeNull();

    await act(async () => resolve(cache));
    expect(screen.getByText('ready')).toBeTruthy();
  });

  it('passes options straight through to createCache', async () => {
    const createCache = vi.fn(async () => cache);
    setCreateCache(createCache as never);
    const options = { persistence: true, broadcastChannel: 'app' };

    await act(async () => {
      render(
        <RecachedProvider options={options}>
          <span>ready</span>
        </RecachedProvider>,
      );
    });
    expect(createCache).toHaveBeenCalledWith(options);
  });

  it('throws a nameable error when a hook is used outside the provider', () => {
    // React logs the thrown error; silence it so the run stays readable.
    const spy = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => renderHook(() => useRecached())).toThrow(/must be called inside a <RecachedProvider>/);
    spy.mockRestore();
  });
});

describe('useKey', () => {
  it('reads the current value and re-renders on mutation', () => {
    cache.seed('theme', 'dark');
    const { result } = renderHook(() => useKey('theme'), { wrapper });
    expect(result.current).toBe('dark');

    act(() => {
      cache.seed('theme', 'light');
      cache.emit();
    });
    expect(result.current).toBe('light');
  });

  it('returns null for a missing key', () => {
    const { result } = renderHook(() => useKey('nope'), { wrapper });
    expect(result.current).toBeNull();
  });

  it('reports a binary value as absent instead of crashing the tree', () => {
    // `get` throws on non-UTF-8. Inside getSnapshot that would unmount the app
    // over a value the component simply cannot display.
    cache.get.mockImplementation(() => {
      throw new Error('not valid UTF-8');
    });
    const { result } = renderHook(() => useKey('bin'), { wrapper });
    expect(result.current).toBeNull();
  });

  it('releases its store subscription on unmount', () => {
    const { unmount } = renderHook(() => useKey('k'), { wrapper });
    expect(cache.listenerCount).toBe(1);
    unmount();
    expect(cache.listenerCount).toBe(0);
  });

  it('useKeyJSON parses, and treats invalid JSON as a miss', () => {
    cache.seed('u', '{"id":42}');
    const { result } = renderHook(() => useKeyJSON<{ id: number }>('u'), { wrapper });
    expect(result.current).toEqual({ id: 42 });

    act(() => {
      cache.seed('u', 'not json');
      cache.emit();
    });
    expect(result.current).toBeNull();
  });

  it('useKeyBytes returns the raw bytes', () => {
    cache.seed('b', 'hi');
    const { result } = renderHook(() => useKeyBytes('b'), { wrapper });
    expect(result.current).toEqual(new TextEncoder().encode('hi'));
  });

  // Regression: `getSnapshot` must return a referentially stable value while
  // nothing has changed. `getJSON` parses a fresh object and `getBytes`
  // allocates a fresh array on every call, so returning them raw makes
  // useSyncExternalStore see a change on every render and re-render forever —
  // "Maximum update depth exceeded" on mount, for any key that exists.
  it.each([
    ['useKeyJSON', () => useKeyJSON('u')],
    ['useKeyBytes', () => useKeyBytes('u')],
  ])('%s keeps snapshot identity stable across renders', (_name, hook) => {
    cache.seed('u', '{"id":42}');
    const { result, rerender } = renderHook(hook, { wrapper });
    const first = result.current;
    rerender();
    expect(result.current).toBe(first);

    // …and yields a new value once the underlying data really changes.
    act(() => {
      cache.seed('u', '{"id":43}');
      cache.emit();
    });
    expect(result.current).not.toBe(first);
  });
});

describe('useKeys', () => {
  it('starts a live query on mount and stops it on unmount', () => {
    const stop = vi.fn();
    cache.liveQuery.mockReturnValue(stop);

    const { unmount } = renderHook(() => useKeys('cart:*'), { wrapper });
    expect(cache.liveQuery).toHaveBeenCalledWith('cart:*');
    expect(stop).not.toHaveBeenCalled();

    unmount();
    expect(stop).toHaveBeenCalledTimes(1);
  });

  it('re-subscribes when the pattern changes', () => {
    const stop = vi.fn();
    cache.liveQuery.mockReturnValue(stop);

    const { rerender } = renderHook(({ p }) => useKeys(p), {
      wrapper,
      initialProps: { p: 'a:*' },
    });
    rerender({ p: 'b:*' });

    expect(stop).toHaveBeenCalledTimes(1);
    expect(cache.liveQuery).toHaveBeenNthCalledWith(2, 'b:*');
  });

  it('returns the same array reference while contents are unchanged', () => {
    // getMatching allocates a fresh array every call. Returning it raw from
    // getSnapshot makes useSyncExternalStore see a new value on every render
    // and loop forever, so the hook memoises on content.
    cache.getMatching.mockImplementation(() => [['p:1', 'x']]);
    const { result } = renderHook(() => useKeys('p:*'), { wrapper });
    const first = result.current;

    act(() => cache.emit());
    expect(result.current).toBe(first);
  });

  it('returns a new array once the contents actually change', () => {
    let pairs: Array<[string, string | Uint8Array | null]> = [['p:1', 'x']];
    cache.getMatching.mockImplementation(() => [...pairs]);
    const { result } = renderHook(() => useKeys('p:*'), { wrapper });
    const first = result.current;

    act(() => {
      pairs = [['p:1', 'y']];
      cache.emit();
    });
    expect(result.current).not.toBe(first);
    expect(result.current).toEqual([['p:1', 'y']]);
  });
});

describe('usePubSub', () => {
  it('subscribes on mount and unsubscribes on unmount', () => {
    const { unmount } = renderHook(() => usePubSub('alerts', vi.fn()), { wrapper });
    expect(cache.subscribe).toHaveBeenCalledWith('alerts');
    expect(cache.unsubscribe).not.toHaveBeenCalled();

    unmount();
    expect(cache.unsubscribe).toHaveBeenCalledWith('alerts');
  });

  it('delivers messages for its channel only', () => {
    const handler = vi.fn();
    renderHook(() => usePubSub('alerts', handler), { wrapper });

    act(() => cache.deliver('alerts', 'ping'));
    expect(handler).toHaveBeenCalledWith('ping');

    act(() => cache.deliver('other', 'nope'));
    expect(handler).toHaveBeenCalledTimes(1);
  });

  it('calls the latest handler without resubscribing', () => {
    // The handler is held in a ref precisely so an inline arrow function —
    // a new identity every render — does not tear down the subscription.
    const first = vi.fn();
    const second = vi.fn();
    const { rerender } = renderHook(({ h }) => usePubSub('alerts', h), {
      wrapper,
      initialProps: { h: first },
    });

    rerender({ h: second });
    act(() => cache.deliver('alerts', 'ping'));

    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledWith('ping');
    expect(cache.subscribe).toHaveBeenCalledTimes(1);
  });
});
