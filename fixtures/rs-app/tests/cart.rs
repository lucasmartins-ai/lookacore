use shop::cart::Cart;

#[test]
fn total_applies_discount() {
    let mut c = Cart::new();
    c.add(1000);
    c.add(500);
    assert_eq!(c.total(10), 1350);
}
