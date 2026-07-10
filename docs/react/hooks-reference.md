# Hooks Reference

## `<RecachedProvider>`

```tsx
<RecachedProvider options={CacheOptions}>
  {children}
</RecachedProvider>
```

Context provider that initializes the cache and makes it available to all descendant hooks. Mount it once near the root of your app.

Renders `null` until the cache is ready (WASM init + optional persistence hydration), then renders `children`.

### Props

| Prop | Type | Description |
|------|------|-------------|
| `options` | `CacheOptions` | Passed directly to `createCache`. Controls persistence, BroadcastChannel, and server connection. |
| `cache` | `Cache` | A pre-built `Cache` instance. When provided, `options` is ignored and initialization is skipped. |
| `children` | `ReactNode` | Your app tree. Rendered only after the cache is ready. |

### Examples

```tsx
// With server connection
<RecachedProvider options={{ connect: { url: 'ws://localhost:6380' } }}>

// With persistence only (no server)
<RecachedProvider options={{ persistence: true }}>

// With cross-tab sync only
<RecachedProvider options={{ broadcastChannel: 'my-app' }}>

// Full setup
<RecachedProvider
  options={{
    persistence: true,
    broadcastChannel: 'my-app',
    connect: { url: 'wss://cache.example.com', password: 'secret' },
  }}
>
```

---

## `useRecached()`

```ts
function useRecached(): Cache
```

Returns the `Cache` instance from the nearest `<RecachedProvider>`. Use it to call write and imperative methods: `set`, `setEx`, `setJSON`, `del`, `publish`, `subscribe`, `clearPersistence`.

Throws if called outside a `<RecachedProvider>`.

### Example

```tsx
function SaveButton() {
  const cache = useRecached()

  async function save() {
    cache.setJSON('user:42', { id: 42, name: 'Alice' }, 300)
  }

  return <button onClick={save}>Save</button>
}
```

### Writing from event handlers

Reads (`useKey`, `useKeyJSON`) are reactive. Writes are always done imperatively via `useRecached()`. This keeps the component API simple: read reactively, write explicitly.

```tsx
function Counter() {
  const cache = useRecached()
  const count = useKey('count')
  const n = Number(count ?? 0)

  return (
    <div>
      <p>{n}</p>
      <button onClick={() => cache.set('count', String(n + 1))}>+</button>
      <button onClick={() => cache.set('count', String(n - 1))}>−</button>
    </div>
  )
}
```

---

## `useKey(key)`

```ts
function useKey(key: string): string | null
```

Reactively reads a string value from the cache. Returns `null` when the key does not exist or has expired.

The component re-renders automatically whenever `key` is mutated — from any source:

- A write in the same component or another component in the same tab
- A write received from the server (WebSocket fan-out)
- A write from another tab (BroadcastChannel sync)

Built on React 18's [`useSyncExternalStore`](https://react.dev/reference/react/useSyncExternalStore) — safe with concurrent rendering and Strict Mode.

### Example

```tsx
function StatusBadge() {
  const status = useKey('system:status')
  return <span className={`badge badge--${status ?? 'unknown'}`}>{status ?? 'unknown'}</span>
}
```

### Dynamic keys

The `key` argument is tracked. Changing it reads the new key immediately:

```tsx
function UserStatus({ userId }: { userId: string }) {
  const status = useKey(`user:${userId}:status`)
  return <span>{status ?? 'offline'}</span>
}
```

### SSR

On the server, `useKey` always returns `null` (there is no WASM store server-side). Handle it like any initially-absent value:

```tsx
const theme = useKey('theme') ?? 'light'
```

---

## `useKeyJSON<T>(key)`

```ts
function useKeyJSON<T>(key: string): T | null
```

Same as `useKey` but JSON-parses the stored string. Returns `null` when the key is missing, expired, or the stored value is not valid JSON.

### Example

```tsx
interface CartItem {
  id: string
  name: string
  qty: number
  price: number
}

function CartSummary() {
  const cache = useRecached()
  const items = useKeyJSON<CartItem[]>('cart') ?? []
  const total = items.reduce((sum, item) => sum + item.price * item.qty, 0)

  function removeItem(id: string) {
    cache.setJSON('cart', items.filter((i) => i.id !== id))
  }

  return (
    <div>
      <ul>
        {items.map((item) => (
          <li key={item.id}>
            {item.name} × {item.qty}
            <button onClick={() => removeItem(item.id)}>Remove</button>
          </li>
        ))}
      </ul>
      <p>Total: ${total.toFixed(2)}</p>
    </div>
  )
}
```

### Writing JSON

Use `cache.setJSON<T>()` from `useRecached()` to write structured data back:

```tsx
cache.setJSON<CartItem[]>('cart', updatedItems)          // no expiry
cache.setJSON<CartItem[]>('cart', updatedItems, 3600)    // expires in 1 hour
```

---

## `useKeys(pattern)`

```ts
function useKeys(pattern: string): Array<[key: string, value: string | null]>
```

Live query: reactively read **every key matching a glob pattern**. On mount, the server sends the current state of all matching keys (merged into the local WASM store), then streams every change to matching keys — including keys created after subscribing. The component re-renders on each change; reads never leave the browser.

Pairs are sorted by key. Values are strings; keys holding collection types come back as `null` (read those with typed accessors). The server subscription is ref-counted per pattern and ends when the last component using it unmounts.

Under [strict sync scoping](/server/sync-scopes), the pattern must sit inside the connection's granted scopes.

### Example

```tsx
function CartList() {
  const items = useKeys('cart:42:item:*')
  return (
    <ul>
      {items.map(([key, qty]) => (
        <li key={key}>
          {key.split(':').pop()}: {qty}
        </li>
      ))}
    </ul>
  )
}
```

Any write to a matching key — from this tab, another tab, the backend over TCP, or another user's browser — re-renders `CartList` with the new data. No fetching, no invalidation, no glue.

## Reactivity model

Every mutation — regardless of source — fires the internal notification bus, which triggers all `useKey` and `useKeyJSON` subscribers to re-read their key and schedule a re-render if the value changed.

```
cache.set('key', value)           ← local write
  │
  ├─ WASM store updated
  ├─ notify all useKey('key') subscribers → re-render
  ├─ WebSocket send → server → fan-out to other clients
  │     └─ WASM store updated → notify → re-render
  └─ BroadcastChannel post → other tabs
        └─ WASM store updated → notify → re-render
```

`useSyncExternalStore` ensures React reads a consistent snapshot — no tearing, no stale closures, compatible with concurrent features like `<Suspense>` and `startTransition`.

---

## TypeScript

All hooks are fully typed. `useKeyJSON<T>` infers the generic `T` from the type argument:

```ts
const user = useKeyJSON<User>('user:42')   // User | null
const items = useKeyJSON<string[]>('tags') // string[] | null
```

`useRecached()` returns the full `Cache` type with all methods typed.
