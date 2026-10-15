/** Applies a percentage discount (capped at 100%) to a price in cents. */
export function applyDiscount(cents: number, percent: number): number {
  return cents - Math.floor((cents * Math.min(percent, 100)) / 100);
}
