from shop.cart import Cart


def test_total_applies_discount():
    c = Cart()
    c.add(1000)
    c.add(500)
    assert c.total(10) == 1350
