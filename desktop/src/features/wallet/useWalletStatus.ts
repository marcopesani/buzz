import { useCallback, useEffect, useRef, useState } from "react";

import { reconcileAndFlushReceipts } from "./reconcileWalletReceipts";
import {
  getWalletStatusCache,
  getWalletStatusEpoch,
  type WalletStatusCache,
} from "./walletState";
import { fetchWalletStatus } from "./walletApi";

const UNLINKED: WalletStatusCache = {
  linked: false,
  capabilities: [],
  receiveMode: "unavailable",
  lud16: null,
  balanceMsat: null,
};

/**
 * Community-scoped wallet status for settings + pay cards.
 * Refetches when the cache epoch bumps (link/unlink/reset).
 */
export function useWalletStatus() {
  const [status, setStatus] = useState<WalletStatusCache>(
    () => getWalletStatusCache() ?? UNLINKED,
  );
  const [loading, setLoading] = useState(() => getWalletStatusCache() == null);
  const [error, setError] = useState<string | null>(null);
  const seenEpoch = useRef(getWalletStatusEpoch());

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const next = await fetchWalletStatus();
      setStatus(next);
      seenEpoch.current = getWalletStatusEpoch();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setStatus(getWalletStatusCache() ?? UNLINKED);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // Poll epoch so invalidateWalletStatus() / community reset is observed.
  useEffect(() => {
    const id = window.setInterval(() => {
      const current = getWalletStatusEpoch();
      if (current !== seenEpoch.current) {
        seenEpoch.current = current;
        void refresh();
      }
    }, 500);
    return () => window.clearInterval(id);
  }, [refresh]);

  // Piggyback receipt outbox flush on the existing wallet-status cadence
  // (mount / link) — no dedicated timer. Only when linked.
  useEffect(() => {
    if (!status.linked) return;
    void reconcileAndFlushReceipts();
  }, [status.linked]);

  return { status, loading, error, refresh };
}
