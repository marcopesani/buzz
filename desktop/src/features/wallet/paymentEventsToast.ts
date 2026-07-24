/**
 * Verified-payment toast: listens to paymentEvents and fires sonner.
 *
 * Policy: toast-only (no OS desktop notification). Mention notifications use a
 * heavier AppShell path; reusing it here would couple wallet settlement to
 * notification permission / sound settings for little gain.
 *
 * Dedupe: the registry's verifiedIds is the source of truth — emit happens
 * once per request. Toast id is a secondary guard against StrictMode double
 * subscribe during remount.
 *
 * No navigate-on-click: there is no existing channel-navigation-from-toast
 * pattern to reuse.
 */

import { toast } from "sonner";

import { msatToSatsDisplay } from "./msat";
import {
  type PaymentEvent,
  resetPaymentEvents,
  subscribePaymentEvents,
} from "./paymentEvents";
import { isWalletExperimentEnabled } from "./walletFeatureEnabled";

let unsubscribe: (() => void) | null = null;

function onPaymentEvent(event: PaymentEvent): void {
  switch (event.type) {
    case "request_paid_verified": {
      const sats = msatToSatsDisplay(event.amountMsat);
      toast.success(`Payment received — ${sats.toLocaleString()} sats`, {
        id: `payment-received-${event.requestEventId}`,
        description: "Verified against your wallet",
      });
      return;
    }
    case "request_expired":
      return;
    default: {
      const _exhaustive: never = event;
      void _exhaustive;
    }
  }
}

/**
 * Install the toast subscriber once (idempotent).
 * No-op when the wallet experiment is off — safe at every call site.
 * Returns whether a subscriber is installed after the call.
 */
export function ensurePaymentEventsToast(
  isEnabled: () => boolean = isWalletExperimentEnabled,
): boolean {
  if (!isEnabled()) return unsubscribe != null;
  if (unsubscribe) return true;
  unsubscribe = subscribePaymentEvents(onPaymentEvent);
  return true;
}

/**
 * Drop the toast subscriber on community switch. Re-installed by
 * `initPendingPaymentRequests` / `ensurePaymentEventsToast`.
 */
export function resetPaymentEventsToast(): void {
  unsubscribe?.();
  unsubscribe = null;
  resetPaymentEvents();
}
