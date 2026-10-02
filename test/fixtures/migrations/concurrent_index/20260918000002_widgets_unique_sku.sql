-- no-transaction
CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS widgets_sku ON widgets (sku);
