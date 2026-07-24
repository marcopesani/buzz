/**
 * Community-scoped durable pending-payment-request registry (localStorage).
 *
 * Same persistence pattern as receiptOutboxStore: TS-side localStorage keyed by
 * community, module memory reset on community switch via
 * `resetPendingPaymentRequests`. Durable buckets stay per-community.
 */

import { loadActiveCommunityId } from "@/features/communities/communityStorage";
import { setLocalStorageItemWithRecovery } from "@/shared/lib/localStorageQuota";

import {
  emptyPendingPaymentRequests,
  type PendingPaymentRequestEntry,
  type PendingPaymentRequestsState,
} from "./pendingPaymentRequests";

const STORAGE_KEY_PREFIX = "buzz-wallet-pending-payment-requests.v1";
/** Bound verifiedIds growth across restarts (FIFO trim on init). */
const MAX_VERIFIED_IDS = 200;

let currentCommunityId = "";
let memCache: PendingPaymentRequestsState | null = null;

/** Bind to the active community when callers race ahead of useCommunityInit. */
function ensureBound(): void {
  if (currentCommunityId) return;
  const id = loadActiveCommunityId();
  if (id) {
    initPendingPaymentRequests(id);
  }
}

function storageKey(communityId: string): string {
  return `${STORAGE_KEY_PREFIX}:${communityId}`;
}

function isValidEntry(value: unknown): value is PendingPaymentRequestEntry {
  if (!value || typeof value !== "object") return false;
  const e = value as Record<string, unknown>;
  return (
    typeof e.requestEventId === "string" &&
    typeof e.channelId === "string" &&
    typeof e.amountMsat === "number" &&
    typeof e.bolt11 === "string" &&
    typeof e.paymentHash === "string" &&
    typeof e.createdAtUnix === "number" &&
    typeof e.expiryUnix === "number" &&
    (e.lastUnconfirmableAtUnix === null ||
      typeof e.lastUnconfirmableAtUnix === "number")
  );
}

function parseStored(raw: string | null): PendingPaymentRequestsState {
  if (!raw) return emptyPendingPaymentRequests();
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") {
      return emptyPendingPaymentRequests();
    }
    const obj = parsed as Record<string, unknown>;
    const pending = Array.isArray(obj.pending)
      ? obj.pending.filter(isValidEntry)
      : [];
    const verifiedIds = Array.isArray(obj.verifiedIds)
      ? obj.verifiedIds.filter((k): k is string => typeof k === "string")
      : [];
    return { pending, verifiedIds };
  } catch {
    return emptyPendingPaymentRequests();
  }
}

function persist(state: PendingPaymentRequestsState): void {
  if (!currentCommunityId) return;
  setLocalStorageItemWithRecovery(
    storageKey(currentCommunityId),
    JSON.stringify(state),
  );
}

/**
 * Bind the registry to a community (loads durable state into memory).
 * Called from `useCommunityInit` after apply.
 */
export function initPendingPaymentRequests(communityId: string): void {
  if (currentCommunityId !== communityId) {
    memCache = null;
  }
  currentCommunityId = communityId;
  if (!memCache) {
    memCache = parseStored(localStorage.getItem(storageKey(communityId)));
    if (memCache.verifiedIds.length > MAX_VERIFIED_IDS) {
      memCache = {
        ...memCache,
        verifiedIds: memCache.verifiedIds.slice(-MAX_VERIFIED_IDS),
      };
      persist(memCache);
    }
  }
}

/**
 * Drop in-memory state on community switch. Durable keys stay per-community.
 * Wired into `resetCommunityState()`.
 */
export function resetPendingPaymentRequests(): void {
  currentCommunityId = "";
  memCache = null;
}

/** Current community's registry (empty when unbound). */
export function getPendingPaymentRequestsState(): PendingPaymentRequestsState {
  ensureBound();
  if (!currentCommunityId) return emptyPendingPaymentRequests();
  if (!memCache) {
    memCache = parseStored(
      localStorage.getItem(storageKey(currentCommunityId)),
    );
  }
  return memCache;
}

/** Replace + persist. No-op when unbound (community teardown). */
export function setPendingPaymentRequestsState(
  state: PendingPaymentRequestsState,
): void {
  ensureBound();
  if (!currentCommunityId) return;
  memCache = state;
  persist(state);
}

/** Read durable state for a community without mutating the active binding. */
export function peekPendingPaymentRequestsForCommunity(
  communityId: string,
): PendingPaymentRequestsState {
  return parseStored(localStorage.getItem(storageKey(communityId)));
}

/** Test helper: write durable state for a community without activating it. */
export function writePendingPaymentRequestsForCommunity(
  communityId: string,
  state: PendingPaymentRequestsState,
): void {
  setLocalStorageItemWithRecovery(
    storageKey(communityId),
    JSON.stringify(state),
  );
}
