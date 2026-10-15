def apply_discount(cents: int, percent: int) -> int:
    """Applies a percentage discount (capped at 100%) to a price in cents."""
    return cents - cents * min(percent, 100) // 100
