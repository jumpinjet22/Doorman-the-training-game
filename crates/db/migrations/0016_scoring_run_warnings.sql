-- Settings a scoring run used that switched a defence off (for example the
-- soft-launch profile's new_account_hours = 0), in words, so a profile left
-- on after launch shows up in the run log and not only in the cron's output.
ALTER TABLE scoring_runs ADD COLUMN warnings jsonb NOT NULL DEFAULT '[]';
