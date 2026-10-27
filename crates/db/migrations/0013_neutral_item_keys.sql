-- Dorman owns its data format now: items are keyed by `item_key`, carry an
-- `intended_intent` (what the generator was asked to write), and no longer
-- hold an external judge's scores.
-- (Skipped if already renamed, so the migration is safe to re-run.)
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM information_schema.columns
               WHERE table_name = 'items' AND column_name = 'knock_key') THEN
        ALTER TABLE items RENAME COLUMN knock_key TO item_key;
    END IF;
END $$;

-- Rewrite the stored payloads to match, replies included.
UPDATE items SET payload =
    (payload - 'knock_key' - 'generator_intent' - 'judge')
    || jsonb_strip_nulls(jsonb_build_object(
        'item_key', payload -> 'knock_key',
        'intended_intent', payload -> 'generator_intent'));

UPDATE items SET payload = jsonb_set(
    payload,
    '{replies}',
    (SELECT coalesce(jsonb_agg((r - 'knock_key' - 'judge')
                               || jsonb_strip_nulls(jsonb_build_object('item_key', r -> 'knock_key'))),
              '[]'::jsonb)
     FROM jsonb_array_elements(payload -> 'replies') AS r))
WHERE jsonb_typeof(payload -> 'replies') = 'array';

-- The scorer no longer writes an export file; results stay in the database.
ALTER TABLE scoring_runs ALTER COLUMN export_path DROP NOT NULL;
