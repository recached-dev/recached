import { onUnmounted } from 'vue';
import { useRecached } from './plugin';

/**
 * Subscribe to a Recached pub/sub channel for the lifetime of the component.
 *
 * Sends `SUBSCRIBE` to the server on setup and `UNSUBSCRIBE` on `onUnmounted`.
 * The `handler` is called with each incoming message. A publisher may send
 * binary, in which case the payload arrives as a `Uint8Array` rather than a
 * string — narrow the type before treating it as text.
 *
 * ```vue
 * <script setup lang="ts">
 * import { usePubSub } from '@recached/vue'
 *
 * usePubSub('alerts', (msg) => {
 *   console.log('New alert:', msg)
 * })
 * </script>
 * ```
 *
 * @param channel  The pub/sub channel name to subscribe to.
 * @param handler  Called with each message payload.
 */
export function usePubSub(
  channel: string,
  handler: (msg: string | Uint8Array) => void,
): void {
  const cache = useRecached();
  cache.subscribe(channel);
  const unsub = cache.onMessage(channel, handler);
  onUnmounted(() => {
    unsub();
    cache.unsubscribe(channel);
  });
}
