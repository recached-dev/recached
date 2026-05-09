import { useEffect } from 'react';
import { useRecached } from './context';

/**
 * Subscribe to a Recached pub/sub channel for the lifetime of the component.
 *
 * Sends `SUBSCRIBE` to the server on mount and `UNSUBSCRIBE` on unmount.
 * The `handler` is called with each incoming message string.
 *
 * ```tsx
 * function Notifications() {
 *   usePubSub('alerts', (msg) => {
 *     console.log('New alert:', msg);
 *   });
 *   return null;
 * }
 * ```
 *
 * @param channel  The pub/sub channel name to subscribe to.
 * @param handler  Called with each message payload. Identity need not be stable
 *                 across renders — the hook captures the latest ref internally.
 */
export function usePubSub(channel: string, handler: (msg: string) => void): void {
  const cache = useRecached();

  useEffect(() => {
    cache.subscribe(channel);
    const unsub = cache.onMessage(channel, handler);
    return () => {
      unsub();
      cache.unsubscribe(channel);
    };
  // channel is the stable dependency; handler intentionally excluded to
  // avoid re-subscribing on every render when defined inline
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [channel]);
}
