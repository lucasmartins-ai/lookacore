from shop.pricing import apply_discount


class Cart:
    def __init__(self) -> None:
        self.items: list[int] = []

    def add(self, cents: int) -> None:
        self.items.append(cents)

    def total(self, discount_percent: int = 0) -> int:
        return apply_discount(sum(self.items), discount_percent)
