-- One row per scored day, written by dorman-scorer in the same transaction as
-- the trust and consensus it saved, so a row here means the run finished.
-- Scoring a day again replaces its row.
CREATE TABLE scoring_runs (
    day          date PRIMARY KEY,
    started_at   timestamptz NOT NULL,
    finished_at  timestamptz NOT NULL,
    players      integer NOT NULL,
    items        integer NOT NULL,
    settled      integer NOT NULL,
    disputed     integer NOT NULL,
    -- Items where a brigade of new accounts had its votes zeroed.
    brigaded     integer NOT NULL,
    export_path  text NOT NULL,
    -- The thresholds the run used (dorman_scorer.config.Config).
    config       jsonb NOT NULL
);
