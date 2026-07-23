/**
 * Amount display boundary helpers.
 *
 * Domain / IPC always speak millisatoshis. Sats exist only here, at the UI edge.
 * Integer division — never floats.
 */

/** Convert msat → whole sats for display (floor). */
export function msatToSatsDisplay(msat: number): number {
  if (!Number.isFinite(msat) || msat < 0) {
    return 0;
  }
  return Math.floor(msat / 1000);
}

/** Convert a whole-sats user input to msat for IPC. */
export function satsToMsat(sats: number): number {
  if (!Number.isFinite(sats) || sats < 0) {
    return 0;
  }
  return Math.floor(sats) * 1000;
}
