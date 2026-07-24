/**
 * Community-scoped durable receipt outbox (localStorage).
 *
 * Same persistence pattern as message drafts: TS-side localStorage keyed by
 * community, module memory reset on community switch via `resetReceiptOutbox`.
 * Durable buckets stay per-community so nothing leaks across switches.
 */

import { loadActiveCommunityId } from "@/features/communities/communityStorage";
import { setLocalStorageItemWithRecovery } from "@/shared/lib/localStorageQuota";

import {
  emptyReceiptOutbox,
  type ReceiptOutboxEntry,
  type ReceiptOutboxState,
} from "./receiptOutbox";

const STORAGE_KEY_PREFIX = "buzz-wallet-receipt-outbox.v1";

let currentCommunityId = "";
let memCache: ReceiptOutboxState | null = null;

/** Bind to the active community when callers race ahead of useCommunityInit. */
function ensureBound(): void {
  if (currentCommunityId) return;
  const id = loadActiveCommunityId();
  if (id) {
    initReceiptOutbox(id);
  }
}

function storageKey(communityId: string): string {
  return `${STORAGE_KEY_PREFIX}:${communityId}`;
}

function isValidEntry(value: unknown): value is ReceiptOutboxEntry {
  if (!value || typeof value !== "object") return false;
  const e = value as Record<string, unknown>;
  return (
    typeof e.requestEventId === "string" &&
    typeof e.paymentHash === "string" &&
    typeof e.preimage === "string" &&
    typeof e.amountMsat === "number" &&
    (e.channelId === null || typeof e.channelId === "string") &&
    typeof e.enqueuedAtUnix === "number" &&
    (e.dropNote === null || typeof e.dropNote === "string") &&
    (e.lastError === null || typeof e.lastError === "string")
  );
}

function parseStored(raw: string | null): ReceiptOutboxState {
  if (!raw) return emptyReceiptOutbox();
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== "object") return emptyReceiptOutbox();
    const obj = parsed as Record<string, unknown>;
    const pending = Array.isArray(obj.pending)
      ? obj.pending.filter(isValidEntry)
      : [];
    const publishedKeys = Array.isArray(obj.publishedKeys)
      ? obj.publishedKeys.filter((k): k is string => typeof k === "string")
      : [];
    return { pending, publishedKeys };
  } catch {
    return emptyReceiptOutbox();
  }
}

function persist(state: ReceiptOutboxState): void {
  if (!currentCommunityId) return;
  setLocalStorageItemWithRecovery(
    storageKey(currentCommunityId),
    JSON.stringify(state),
  );
}

/**
 * Bind the outbox to a community (loads durable state into memory).
 * Called from `useCommunityInit` after apply.
 */
export function initReceiptOutbox(communityId: string): void {
  if (currentCommunityId !== communityId) {
    memCache = null;
  }
  currentCommunityId = communityId;
  if (!memCache) {
    memCache = parseStored(localStorage.getItem(storageKey(communityId)));
  }
}

/**
 * Drop in-memory state on community switch. Durable keys stay per-community.
 * Wired into `resetCommunityState()`.
 */
export function resetReceiptOutbox(): void {
  currentCommunityId = "";
  memCache = null;
}

/** Current community's outbox (empty when unbound). */
export function getReceiptOutboxState(): ReceiptOutboxState {
  ensureBound();
  if (!currentCommunityId) return emptyReceiptOutbox();
  if (!memCache) {
    memCache = parseStored(
      localStorage.getItem(storageKey(currentCommunityId)),
    );
  }
  return memCache;
}

/** Replace + persist. No-op when unbound (community teardown). */
export function setReceiptOutboxState(state: ReceiptOutboxState): void {
  ensureBound();
  if (!currentCommunityId) return;
  memCache = state;
  persist(state);
}

/** Read durable state for a community without mutating the active binding. */
export function peekReceiptOutboxForCommunity(
  communityId: string,
): ReceiptOutboxState {
  return parseStored(localStorage.getItem(storageKey(communityId)));
}

/** Test helper: write durable state for a community without activating it. */
export function writeReceiptOutboxForCommunity(
  communityId: string,
  state: ReceiptOutboxState,
): void {
  setLocalStorageItemWithRecovery(
    storageKey(communityId),
    JSON.stringify(state),
  );
}
