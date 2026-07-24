/**
 * Tiny module-level pub/sub for wallet-verified payment lifecycle events.
 *
 * Emitters: the single verify path (receipt nudge + poll cadence).
 * Consumers: verified toast; exported for future automations.
 * Reset with community state (listeners stay; they re-bind to the new community
 * via the emptied registry — no cross-community verified events fire).
 */

export type PaymentEvent =
  | {
      type: "request_paid_verified";
      requestEventId: string;
      channelId: string;
      amountMsat: number;
    }
  | {
      type: "request_expired";
      requestEventId: string;
      channelId: string;
      amountMsat: number;
    };

type PaymentEventListener = (event: PaymentEvent) => void;

const listeners = new Set<PaymentEventListener>();

/** Subscribe to verified / expired payment events. Returns unsubscribe. */
export function subscribePaymentEvents(
  listener: PaymentEventListener,
): () => void {
  listeners.add(listener);
  return () => {
    listeners.delete(listener);
  };
}

/** Emit a typed payment event to all subscribers. */
export function emitPaymentEvent(event: PaymentEvent): void {
  for (const listener of listeners) {
    listener(event);
  }
}

/**
 * Community-switch reset. Clears listeners so a remounted toast effect can
 * re-subscribe cleanly (mirrors other community-scoped signal resets).
 */
export function resetPaymentEvents(): void {
  listeners.clear();
}
