use crate::pricing::apply_discount;

#[derive(Default)]
pub struct Cart {
    items: Vec<u64>,
}

impl Cart {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, cents: u64) {
        self.items.push(cents);
    }

    pub fn total(&self, discount_percent: u8) -> u64 {
        apply_discount(self.items.iter().sum(), discount_percent)
    }
}
