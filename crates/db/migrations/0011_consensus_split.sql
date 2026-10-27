-- The weighted split behind each verdict, for yesterday's reveal page:
-- answer -> total trust of the players who gave it (pair items use the
-- reply knock_keys and "both_bad"). Votes zeroed as a brigade are left out.
-- The live split during the day stays raw counts from `votes`.
ALTER TABLE consensus ADD COLUMN split jsonb NOT NULL DEFAULT '{}';
