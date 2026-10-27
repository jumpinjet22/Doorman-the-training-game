-- Every loaded batch waits for a human skim (`dorman-ingest review`, then
-- `approve`) before any of its items can go into a daily set.
-- No foreign key from items.batch: an item whose batch has no row here is
-- simply never approved.

CREATE TABLE batches (
    name         text PRIMARY KEY,
    status       text NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'approved')),
    loaded_at    timestamptz NOT NULL DEFAULT now(),
    approved_at  timestamptz NULL
);

-- Batches loaded before the gate existed were already being played.
INSERT INTO batches (name, status, approved_at)
SELECT DISTINCT batch, 'approved', now() FROM items;
