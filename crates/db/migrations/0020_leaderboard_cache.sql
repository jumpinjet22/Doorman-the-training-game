-- The rendered public leaderboard, so a flood of requests runs its weekly
-- aggregate at most once a minute instead of once per request. One row per
-- week; a page older than the cache window is recomputed and replaced.
-- Kept in the database rather than in memory so every API instance shares it.
CREATE TABLE leaderboard_cache (
    week_of      date PRIMARY KEY,
    computed_at  timestamptz NOT NULL,
    body         text NOT NULL
);
