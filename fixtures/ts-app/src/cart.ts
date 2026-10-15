import { applyDiscount } from "./pricing.ts";

export class Cart {
  private items: number[] = [];

  add(cents: number): void {
    this.items.push(cents);
  }

  total(discountPercent = 0): number {
    return applyDiscount(this.items.reduce((a, b) => a + b, 0), discountPercent);
  }
}
