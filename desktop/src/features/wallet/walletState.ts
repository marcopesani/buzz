/**
 * Community-scoped wallet frontend cache.
 *
 * The NWC secret never lives here — only status / invalidation signals that
 * U11 UI will consume. Must be reset on community switch via
 * `resetCommunityState()`.
 */

export type WalletStatusCache = {
  linked: boolean;
  capabilities: string[];
  receiveMode: string;
  lud16: string | null;
  balanceMsat: number | null;
};

let statusCache: WalletStatusCache | null = null;
let statusEpoch = 0;

/** Bump when native wallet status should be re-fetched. */
export function invalidateWalletStatus(): void {
  statusCache = null;
  statusEpoch += 1;
}

/** Last known status snapshot (null until populated by U11). */
export function getWalletStatusCache(): WalletStatusCache | null {
  return statusCache;
}

/** Replace the status cache (called by U11 after `wallet_status`). */
export function setWalletStatusCache(status: WalletStatusCache): void {
  statusCache = status;
}

/** Monotonic epoch — subscribers can detect invalidation. */
export function getWalletStatusEpoch(): number {
  return statusEpoch;
}

/**
 * Drop all community-scoped wallet frontend state.
 * Wired into `resetCommunityState()` in useCommunityInit.
 */
export function resetWalletState(): void {
  statusCache = null;
  statusEpoch += 1;
}
