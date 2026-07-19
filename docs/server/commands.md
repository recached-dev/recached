# Commands

Recached implements the subset of RESP commands that most applications use. Commands work over both TCP (port 6379) and WebSocket (port 6380).

## Core

| Command | Description |
|---|---|
| `PING [message]` | Returns `PONG`, or echoes `message` if provided. Used to test connectivity and measure latency. |
| `AUTH password` | Authenticates the connection. Required on the first command if `RECACHED_PASSWORD` is set. 5 consecutive failures close the connection. |
| `HELLO [protover]` | Reports server info and negotiates the protocol version. `3` switches the connection to RESP3, `2` back to RESP2, no argument reports without changing. Unsupported versions return `-NOPROTO` and leave the connection unchanged. Requires authentication. See [Wire Protocol](/server/protocol#protocol-version-tcp). |

---

## Strings

The most common data type. Values are always stored as byte strings; numeric operations parse the value as an integer or float.

| Command | Description |
|---|---|
| `SET key value [EX seconds] [PX ms] [EXAT timestamp] [PXAT ms-timestamp] [NX\|XX] [KEEPTTL] [GET]` | Set a key to a string value. `EX`/`PX`/`EXAT`/`PXAT` set expiry. `NX` only sets if key does not exist. `XX` only sets if key exists. `KEEPTTL` preserves the existing TTL. `GET` returns the old value before overwriting. |
| `GET key` | Returns the value of a key, or nil if the key does not exist or has expired. |
| `GETSET key value` | Sets the key to a new value and returns the old value atomically. Deprecated in Redis 6.2 — prefer `SET key value GET`. |
| `MGET key [key ...]` | Returns the values of multiple keys. Keys that do not exist return nil. |
| `MSET key value [key value ...]` | Sets multiple keys to their respective values in a single atomic operation. |
| `ESET key value` | **Ephemeral set.** Stores a string like `SET`, but the key's lifetime is bound to the connection that wrote it — when that connection closes, the server deletes the key and the deletion is pushed to live queries. Writing the same key again transfers ownership to the newest connection, so a second browser tab keeps presence alive when the first closes. Intended for presence, cursors, and "who is online"; use `SET` for anything that should outlive a connection. |
| `SETNX key value` | Set a key only if it does not exist. Returns 1 if set, 0 if the key already existed. |
| `SETEX key seconds value` | Set a key with an integer-second expiry. Equivalent to `SET key value EX seconds`. |
| `PSETEX key milliseconds value` | Set a key with a millisecond-precision expiry. |
| `APPEND key value` | Appends a string to the end of the existing value. If the key does not exist, it is created. Returns the new length. |
| `STRLEN key` | Returns the length of the string stored at key. Returns 0 if the key does not exist. |
| `INCR key` | Increments the integer value of a key by 1. Creates the key with value 1 if it does not exist. Returns an error if the value is not a valid integer. |
| `DECR key` | Decrements the integer value of a key by 1. Creates the key with value -1 if it does not exist. |
| `INCRBY key increment` | Increments the integer value of a key by the given integer. |
| `DECRBY key decrement` | Decrements the integer value of a key by the given integer. |

---

## Expiry

| Command | Description |
|---|---|
| `EXPIRE key seconds` | Set a timeout on a key in seconds. The key is deleted when the timeout expires. Returns 1 if set, 0 if key does not exist. |
| `PEXPIRE key milliseconds` | Set a timeout in milliseconds. |
| `EXPIREAT key unix-timestamp` | Set an absolute expiry time as a Unix timestamp (seconds). |
| `PEXPIREAT key ms-unix-timestamp` | Set an absolute expiry time as a Unix timestamp in milliseconds. |
| `TTL key` | Returns the remaining time-to-live of a key in seconds. Returns -2 if the key does not exist, -1 if the key has no expiry. |
| `PTTL key` | Returns the remaining TTL in milliseconds. |
| `PERSIST key` | Removes the TTL from a key, making it persistent. Returns 1 if the TTL was removed, 0 if the key has no expiry or does not exist. |

---

## Keys

| Command | Description |
|---|---|
| `DEL key [key ...]` | Deletes one or more keys. Returns the number of keys that were deleted. Keys that do not exist are ignored. |
| `UNLINK key [key ...]` | Non-blocking delete. Semantically equivalent to `DEL` (Recached does not implement async deletion, but `UNLINK` is accepted for client compatibility). |
| `EXISTS key [key ...]` | Returns the number of keys that exist among the provided arguments. A key listed multiple times counts multiple times. |
| `TYPE key` | Returns the type of the value stored at key: `string`, `hash`, `list`, `set`, `zset`, `ratelimit`, or `none` if the key does not exist. |
| `RENAME key newkey` | Renames a key. Returns an error if the source key does not exist. Overwrites `newkey` if it already exists. |
| `KEYS pattern` | Returns all keys matching the glob pattern. `*` matches any sequence of characters, `?` matches a single character, `[abc]` matches a character class. Warning: `KEYS *` on a large store is slow — prefer `SCAN`. |
| `SCAN cursor [MATCH pattern] [COUNT count]` | Iterates keys incrementally, returning at most `COUNT` keys per call (default 10) plus the next cursor. Start with cursor `0` and continue until the returned cursor is `0`. `MATCH` filters results by glob pattern. As in Redis, keys inserted or deleted mid-iteration may be missed or returned twice. |
| `DBSIZE` | Returns the total number of keys in the store. |
| `FLUSHDB [ASYNC]` | Removes all keys from the store. `ASYNC` is accepted but does not change behavior (the flush is always synchronous). |

---

## Hash

A hash is a map of field-value pairs stored under a single key. Use hashes to store structured objects without serializing to JSON.

| Command | Description |
|---|---|
| `HSET key field value [field value ...]` | Sets one or more fields in a hash. Creates the hash if it does not exist. Returns the number of fields that were added (not updated). |
| `HGET key field` | Returns the value of a specific field. Returns nil if the field or hash does not exist. |
| `HGETALL key` | Returns all field-value pairs of a hash as a flat array: field1, value1, field2, value2, ... |
| `HDEL key field [field ...]` | Deletes one or more fields from a hash. Returns the number of fields removed. |
| `HMGET key field [field ...]` | Returns the values of multiple fields. Non-existent fields return nil. |
| `HKEYS key` | Returns all field names in the hash. |
| `HVALS key` | Returns all values in the hash. |
| `HLEN key` | Returns the number of fields in the hash. |
| `HEXISTS key field` | Returns 1 if the field exists in the hash, 0 otherwise. |
| `HSETNX key field value` | Sets a field only if it does not already exist. Returns 1 if set, 0 if the field already existed. |
| `HINCRBY key field increment` | Increments the integer value of a hash field by the given integer. Creates the field with value 0 before incrementing if it does not exist. |
| `HINCRBYFLOAT key field increment` | Increments the float value of a hash field by the given float. |

### Example

```bash
HSET user:1 name Alice plan pro credits 500
HGET user:1 name          # "Alice"
HGETALL user:1            # ["name", "Alice", "plan", "pro", "credits", "500"]
HINCRBY user:1 credits -50
HGET user:1 credits       # "450"
```

---

## List

A doubly-linked list. Supports push/pop from both ends. Use for queues (`RPUSH` + `LPOP`), stacks (`LPUSH` + `LPOP`), and fixed-length histories (`RPUSH` + `LTRIM`).

| Command | Description |
|---|---|
| `LPUSH key element [element ...]` | Prepends one or more elements to the head of a list. Multiple elements are pushed left-to-right (the last argument ends up at the head). Returns the list length. |
| `RPUSH key element [element ...]` | Appends one or more elements to the tail of a list. Returns the list length. |
| `LPUSHX key element [element ...]` | Like `LPUSH`, but only if the key already exists. Returns 0 if the key does not exist. |
| `RPUSHX key element [element ...]` | Like `RPUSH`, but only if the key already exists. |
| `LPOP key [count]` | Removes and returns the first element (or `count` elements) from the list. Returns nil if the list is empty or does not exist. |
| `RPOP key [count]` | Removes and returns the last element (or `count` elements) from the list. |
| `LRANGE key start stop` | Returns the elements of the list between index `start` and `stop` (inclusive). Negative indices count from the tail: -1 is the last element. |
| `LLEN key` | Returns the length of the list, or 0 if the key does not exist. |
| `LINDEX key index` | Returns the element at the given index. 0 is the first element, -1 is the last. Returns nil if the index is out of range. |
| `LSET key index element` | Sets the list element at the given index to a new value. Returns an error if the index is out of range. |
| `LREM key count element` | Removes occurrences of `element` from the list. `count > 0`: removes from head. `count < 0`: removes from tail. `count = 0`: removes all occurrences. Returns the number of removed elements. |
| `LTRIM key start stop` | Keeps only the elements between `start` and `stop`, removing the rest. Useful for capping list length. |

---

## Set

An unordered collection of unique string members. Supports set operations (intersection, union, difference).

| Command | Description |
|---|---|
| `SADD key member [member ...]` | Adds one or more members to a set. Ignores members that already exist. Returns the number of new members added. |
| `SMEMBERS key` | Returns all members of the set. Order is not guaranteed. |
| `SREM key member [member ...]` | Removes one or more members from a set. Returns the number of members actually removed. |
| `SCARD key` | Returns the number of members in the set. |
| `SISMEMBER key member` | Returns 1 if the member exists in the set, 0 otherwise. |
| `SMISMEMBER key member [member ...]` | Returns an array of 1/0 values for each member, indicating existence. |
| `SINTER key [key ...]` | Returns the intersection of all given sets. |
| `SINTERSTORE destination key [key ...]` | Stores the intersection into `destination` and returns its size. |
| `SUNION key [key ...]` | Returns the union of all given sets. |
| `SUNIONSTORE destination key [key ...]` | Stores the union into `destination` and returns its size. |
| `SDIFF key [key ...]` | Returns the members in the first set that are not in any of the other sets. |
| `SDIFFSTORE destination key [key ...]` | Stores the difference into `destination` and returns its size. |
| `SPOP key [count]` | Removes and returns one or more random members from the set. |
| `SRANDMEMBER key [count]` | Returns one or more random members without removing them. Positive `count`: unique members. Negative `count`: may repeat. |
| `SMOVE source destination member` | Atomically moves a member from one set to another. Returns 1 on success, 0 if the member did not exist in source. |

---

## Sorted Set

An ordered collection where each member has a numeric score. Members are unique; scores need not be. Members are always returned in ascending score order.

| Command | Description |
|---|---|
| `ZADD key [NX\|XX] [CH] [INCR] score member [score member ...]` | Adds members with scores. `NX`: only add, never update. `XX`: only update, never add. `CH`: count changed elements (updated + added) instead of just added. `INCR`: add the score to the existing score instead of replacing it. |
| `ZREM key member [member ...]` | Removes members from the sorted set. Returns the number of members removed. |
| `ZINCRBY key increment member` | Adds `increment` to the score of `member`. Creates the member with score `increment` if it does not exist. |
| `ZRANGE key start stop [WITHSCORES]` | Returns members between rank `start` and `stop` (0-based, ascending). Add `WITHSCORES` to include scores. |
| `ZREVRANGE key start stop [WITHSCORES]` | Same as `ZRANGE`, but returns members in descending score order. |
| `ZRANGEBYSCORE key min max [WITHSCORES] [LIMIT offset count]` | Returns members with scores between `min` and `max`. Use `-inf` and `+inf` for open bounds. Use `(min` for exclusive lower bound. |
| `ZREVRANGEBYSCORE key max min [WITHSCORES] [LIMIT offset count]` | Same, but from high score to low. |
| `ZSCORE key member` | Returns the score of a member, or nil if the member does not exist. |
| `ZMSCORE key member [member ...]` | Returns the scores of multiple members. Non-existent members return nil. |
| `ZRANK key member` | Returns the 0-based rank of a member in ascending score order. Returns nil if the member does not exist. |
| `ZREVRANK key member` | Returns the rank in descending score order. |
| `ZCARD key` | Returns the number of members in the sorted set. |
| `ZCOUNT key min max` | Returns the number of members with scores between `min` and `max`. |

### Example: leaderboard

```bash
ZADD leaderboard 1500 alice 2200 bob 980 carol
ZREVRANGE leaderboard 0 2 WITHSCORES
# ["bob", "2200", "alice", "1500", "carol", "980"]

ZINCRBY leaderboard 300 carol
ZREVRANK leaderboard carol    # 2 → 1 (moved up)
```

---

## JSON

A native JSON document type — no RedisJSON module needed. Documents are stored parsed, so path reads and partial updates never re-serialize the whole value, and only the change travels to replicas, the AOF, and connected browsers.

| Command | Description |
|---|---|
| `JSET key path value` | Set JSON at a path. `$` is the whole document, `$.user.name` a nested field, `$.items[2].qty` an array element. Intermediate objects are auto-created; array indices must exist. `value` must be valid JSON text. |
| `JGET key [path]` | Read the JSON at a path (default `$`), serialized. Returns nil when the key or path does not exist. Object keys serialize in sorted order — deterministic output. |
| `JMERGE key patch` | RFC 7386 JSON Merge Patch against the whole document: objects merge recursively, `null` fields are removed, arrays and scalars are replaced. A `null` patch deletes the key. Creates the document if missing. |

Paths address exactly one location — wildcards, slices, and filters are not supported. `TYPE` reports `json`.

### Example: partial updates

```bash
JSET doc:42 $ '{"title":"Draft","meta":{"views":0,"draft":true}}'
JGET doc:42 $.meta.views          # "0"
JSET doc:42 $.meta.views 17       # only this field changes
JMERGE doc:42 '{"title":"Final","meta":{"draft":null}}'
JGET doc:42                       # {"meta":{"views":17},"title":"Final"}
```

The browser SDK exposes the same commands as [`jset` / `jget` / `jmerge`](/browser/api-reference#json-documents) with `JSON.stringify`/`parse` handled for you — a `JMERGE` from any client updates every connected browser's local document.

In live queries (`QSUB` / `useKeys`), JSON keys arrive as `["json", document]` — the document travels with the notification, so no follow-up `JGET` is needed.

---

## Rate Limiting

A built-in sliding-window rate limiter — no INCR+EXPIRE races, no Lua scripts. Internally a limiter key stores its config plus the timestamps of allowed attempts inside the window (type name: `ratelimit`). Denied attempts are not recorded, so a client hammering a full limiter does not push its own recovery further away.

| Command | Description |
|---|---|
| `RLSET key limit window` | Configure a limiter: at most `limit` attempts per `window` seconds. Reconfiguring in place keeps already-recorded attempts. Limiters created with `RLSET` persist until `DEL`/`EXPIRE`. |
| `RLCHECK key [limit window]` | Record an attempt. Returns a 3-element array: `[allowed (1\|0), remaining, retry_after_ms]`. With the optional `limit window` pair, the limiter is created on first use — ideal for per-IP or per-user keys where a separate `RLSET` round-trip per key is impractical. Auto-created limiters self-clean: they expire one window after the last attempt. Bare `RLCHECK` on an unconfigured key returns an error. |

The reply maps directly onto standard HTTP rate-limit headers: `remaining` → `X-RateLimit-Remaining`, `retry_after_ms` → `Retry-After`.

### Example: per-IP request limiting

```bash
# 100 requests per minute per client IP — one command per request,
# limiter auto-created on the first attempt and self-cleans when idle.
RLCHECK ip:203.0.113.7 100 60
# 1) (integer) 1        allowed
# 2) (integer) 99       remaining in window
# 3) (integer) 0        retry_after_ms

# ...101st request within the minute:
# 1) (integer) 0
# 2) (integer) 0
# 3) (integer) 58211    → Retry-After: 59
```

### Example: named app-level limiter

```bash
RLSET login:alice 5 300      # 5 login attempts per 5 minutes
RLCHECK login:alice          # check + record one attempt
```

Replication note: `RLSET` config replicates to AOF and replicas; recorded attempts are transient and deliberately do not (streaming every check would flood the write log for state that expires within one window).

---

## Sync Scoping (WebSocket only)

Controls which keys a WebSocket connection receives pushes for and may operate on. Full guide: [Sync Scopes](/server/sync-scopes).

| Command | Description |
|---|---|
| `SYNC` | Returns this connection's current scope patterns. |
| `SYNC TOKEN token` | Sets scopes from a token signed with `RECACHED_SYNC_SECRET` (HMAC-SHA256). Required before any key access when the secret is configured (strict mode). Returns the granted patterns. |
| `SYNC pattern [pattern ...]` | Sets scopes directly from glob patterns. Only available when no sync secret is configured — a bandwidth filter, not a security boundary. |

On the TCP port, `SYNC` returns an error — backend connections are trusted and unscoped.

### Exactly-once envelope

| Command | Description |
|---|---|
| `DEDUP client-id id command args...` | Wraps a write with a per-client monotonic id. If `id` is at or below the highest id already applied for `client-id`, the write is skipped and the reply is `+DUP` — used by the browser SDK's offline-replay so a write whose acknowledgment was lost never applies twice. Client ids are 1–64 characters and should be unguessable (the SDK uses `crypto.randomUUID()`). Scope checks, replica rejection, and metrics all apply to the wrapped command. Dedup marks are in-memory, per server, swept after 24 h idle. |

---

## Live Queries (WebSocket only)

A live query delivers the current state of every key matching a glob pattern, then streams every subsequent change to matching keys — initial state plus diffs, not fire-and-forget events. This is the primitive behind reactive UI bindings.

| Command | Description |
|---|---|
| `QSUB pattern` | Subscribe. The reply is `["qstate", pattern, key, value, ...]` — the current state of every live key matching the pattern as flat pairs. Afterwards, every mutation to a matching key — including keys created later — arrives as a `["keychange", key, value]` push; deletions arrive with a nil value. Initial state is capped at 10 000 keys. Up to 64 live queries per connection. |
| `QUNSUB [pattern]` | Drop one live query, or all of them without an argument. |

```bash
QSUB cart:42:*
# 1) "qstate"
# 2) "cart:42:*"
# 3) "cart:42:item:9"     initial state…
# 4) "2"
# …then, when the server (or any client) writes cart:42:item:12:
# ["keychange", "cart:42:item:12", "1"]
```

Under strict sync scoping, `QSUB` patterns must sit inside the connection's granted scopes — a grant of `cart:42:*` covers `QSUB cart:42:*` and narrower prefix patterns. Live-query pushes never interfere with `WATCH` transactions (they travel on a separate internal channel).

### Value shapes

Strings arrive as a bulk string and deletions as nil. Collections arrive **type-tagged** — an array
whose first element names the type — so a subscriber can rebuild the value without a follow-up read:

```text
hash  →  ["hash", field, value, ...]     fields sorted
list  →  ["list", element, ...]          head to tail
set   →  ["set", member, ...]
zset  →  ["zset", member, score, ...]    ascending score
json  →  ["json", document]
```

The tag is what makes the payload unambiguous: a four-element array would otherwise be
indistinguishable between a list of four items and a hash of two pairs. Ordering is deterministic, so
two clients receiving the same notification build identical local state.

Each notification carries the **complete** current value, so the receiver replaces the key rather than
merging — which is what allows a removed member to propagate.

`FLUSHDB` is announced as a single sentinel per subscribed pattern — a `keychange` whose key is the
pattern and whose value is nil — meaning "every key matching this pattern is gone". Announcing each
deleted key would mean one frame per key in the keyspace for one command. The browser SDK expands the
sentinel locally; a hand-written client should do the same.

::: warning Changed in 0.2.2
Before 0.2.2 collections arrived as a bare type name (`"hash"`), and subscribers had to follow up with
`HGETALL`/`LRANGE`. Server and SDK are released in lockstep — run matching versions, since a 0.2.1
client ignores the new shape.
:::

---

## Transactions

Transactions queue commands and execute them atomically. No other client can interleave commands between `MULTI` and `EXEC`. After `EXEC`, the full result set is broadcast to WebSocket clients.

| Command | Description |
|---|---|
| `MULTI` | Begins a transaction. Subsequent commands are queued, not executed. Returns `OK`. |
| `EXEC` | Executes all queued commands. Returns an array of results, one per queued command — or a nil array if a `WATCH`ed key changed since `WATCH` was issued (optimistic-lock abort). |
| `DISCARD` | Abandons the transaction queue. Returns `OK`. Also clears any `WATCH`ed keys. |

### Example

```bash
MULTI
SET counter 0
INCR counter
INCR counter
EXEC
# 1) OK
# 2) 1
# 3) 2
```

Optimistic locking: `WATCH key [key ...]` before `MULTI` marks those keys. If any watched key is modified by **any** client before `EXEC`, the transaction is aborted and `EXEC` returns a nil array (Redis `WATCH`/`MULTI`/`EXEC` CAS semantics). This works over both the TCP (6379) and WebSocket (6380) ports. `EXEC` and `DISCARD` both clear all watches. Over WebSocket, `WATCH` *additionally* pushes live keychange notifications — see [Observable Keys](#observable-keys) below.

---

## Pub/Sub

Publish/subscribe messaging. Clients can subscribe to channels (exact match) or patterns (glob). Published messages are delivered to all matching subscribers.

Pub/Sub works over both TCP (port 6379) and WebSocket (port 6380).

| Command | Description |
|---|---|
| `SUBSCRIBE channel [channel ...]` | Subscribes the client to one or more channels. The client enters pub/sub mode and can only use pub/sub commands until it unsubscribes. |
| `UNSUBSCRIBE [channel ...]` | Unsubscribes from the given channels. With no arguments, unsubscribes from all channels. |
| `PSUBSCRIBE pattern [pattern ...]` | Subscribes to channels matching a glob pattern. `*` matches any sequence, `?` matches any single character, `[abc]` matches a character class. |
| `PUNSUBSCRIBE [pattern ...]` | Unsubscribes from pattern subscriptions. With no arguments, unsubscribes from all patterns. |
| `PUBLISH channel message` | Publishes a message to all subscribers of the given channel and all clients with matching pattern subscriptions. Returns the number of clients that received the message. |

### Example

```typescript
// Subscriber (Node.js)
const sub = new Redis('redis://127.0.0.1:6379')
await sub.subscribe('events:orders')
sub.on('message', (channel, message) => {
  console.log(`${channel}: ${message}`)
})

// Publisher
const pub = new Redis('redis://127.0.0.1:6379')
await pub.publish('events:orders', JSON.stringify({ id: 123, status: 'shipped' }))
// events:orders: {"id":123,"status":"shipped"}
```

---

## Replication

Replication topology is set at startup with [`RECACHED_REPLICAOF`](/server/configuration#environment-variable-reference). One runtime command exists, for promoting a replica during failover.

| Command | Description |
|---|---|
| `REPLICAOF NO ONE` | Promotes this replica to a primary: it stops following its upstream and begins accepting writes. This is the **only** accepted form — pointing a running server at a new primary (`REPLICAOF host port`) is rejected with an error. To re-point a server, restart it with a different `RECACHED_REPLICAOF`. |

Automatic promotion on primary failure is configured with `RECACHED_FAILOVER_TIMEOUT`; `REPLICAOF NO ONE` is the manual equivalent. Note that promotion is not coordinated between replicas — see [when Recached is not the right fit](/guide/introduction#when-recached-is-not-the-right-fit) for the split-brain caveat.

---

## Persistence

Snapshot commands write the in-memory store to disk in MessagePack format. The snapshot path and autosave interval are controlled by [`RECACHED_SAVE_PATH` and `RECACHED_SAVE_INTERVAL`](/server/configuration#environment-variable-reference).

| Command | Description |
|---|---|
| `SAVE` | Synchronously writes a snapshot to disk. Blocks until the file is written. Returns `OK` on success. |
| `BGSAVE` | Triggers a background snapshot. Returns immediately; the save runs in a background task while the server continues accepting connections. |
| `LASTSAVE` | Returns the Unix timestamp (seconds) of the most recent successful snapshot. Returns the server start time if no save has completed yet. |

### Example

```bash
# Trigger a background save and check when it completed
BGSAVE          # +Background saving started
# ... time passes ...
LASTSAVE        # (integer) 1746794400
```

```bash
# Force a synchronous save (blocks until done — use BGSAVE in production)
SAVE            # +OK
```

---

## Observable Keys

`WATCH` and `UNWATCH` serve two roles in Recached:

1. **Optimistic locking (both transports).** Over TCP (6379) and WebSocket (6380), `WATCH` participates in `MULTI`/`EXEC` exactly like Redis: if a watched key changes before `EXEC`, the transaction aborts (nil array). See [Transactions](#transactions).
2. **Live change notifications (WebSocket only).** Over WebSocket, `WATCH` *additionally* subscribes the connection to keychange pushes: whenever a watched key is mutated by any client, the server sends a push frame to every watching WS connection. TCP connections receive no such push (it would violate the request/response protocol) — they use `WATCH` purely for the CAS guarantee above.

| Command | Description |
|---|---|
| `WATCH key [key ...]` | Marks the given key(s) for optimistic locking, and (over WebSocket) registers the connection for keychange push notifications. Not allowed once `MULTI` has started. |
| `UNWATCH [key ...]` | Stops watching the given keys. With no arguments, clears all watches for this connection. `EXEC` and `DISCARD` also clear all watches. |

### Push message format

When a watched key changes, the server sends a RESP array:

```
["keychange", "key-name", "new-value-or-type-hint"]
```

- For string keys: the third element is the current value.
- For complex types (hash, list, set, sorted set): the third element is the type name (`hash`, `list`, `set`, `zset`). Re-fetch the full value with `HGETALL`, `LRANGE`, `SMEMBERS`, or `ZRANGE`.
- For deleted keys: the third element is nil (`$-1\r\n`).

### Raw WebSocket example

```javascript
const ws = new WebSocket('ws://127.0.0.1:6380')

ws.onopen = () => {
  // Watch a key — send RESP directly
  ws.send('*2\r\n$5\r\nWATCH\r\n$12\r\ncart:user:42\r\n')
}

ws.onmessage = ({ data }) => {
  // Parse RESP push: ["keychange", "cart:user:42", "3"]
  console.log('Key changed:', data)
}

// Stop watching this key
ws.send('*2\r\n$7\r\nUNWATCH\r\n$12\r\ncart:user:42\r\n')

// Stop watching all keys
ws.send('*1\r\n$7\r\nUNWATCH\r\n')
```

When using the `recached-edge` WASM client, `WATCH`/`UNWATCH` are wrapped in the `cache.watch()` / `cache.unwatch()` TypeScript API — you do not need to handle raw RESP.
