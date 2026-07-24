/**
 * Non-React wallet experiment check — same resolution as useFeatureEnabled.
 */

import { getFeature, getOverrides, resolveEnabled } from "@/shared/features";

/** True when the `wallet` preview feature is on for the active overrides. */
export function isWalletExperimentEnabled(): boolean {
  const feature = getFeature("wallet");
  if (!feature) {
    // Not in manifest → stable / fail-open (mirrors useFeatureEnabled).
    return true;
  }
  return resolveEnabled("wallet", getOverrides(), feature.defaultEnabled);
}
