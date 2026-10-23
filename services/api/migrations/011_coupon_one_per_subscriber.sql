-- One coupon, one subscriber: a coupon code may be claimed by exactly one
-- subscriber, and a subscriber may claim exactly one coupon.
CREATE UNIQUE INDEX IF NOT EXISTS idx_coupon_usages_one_per_coupon
  ON coupon_usages(coupon_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_coupon_usages_one_per_subscriber
  ON coupon_usages(email_normalized);
