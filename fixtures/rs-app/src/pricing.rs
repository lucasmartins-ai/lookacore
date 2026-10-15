/// Applies a percentage discount (capped at 100%) to a price in cents.
pub fn apply_discount(cents: u64, percent: u8) -> u64 {
    cents - cents * u64::from(percent.min(100)) / 100
}
