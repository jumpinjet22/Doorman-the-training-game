-- Handles name players on the weekly leaderboard, so no two may match,
-- whatever their case.
CREATE UNIQUE INDEX players_handle_unique ON players (lower(handle)) WHERE handle IS NOT NULL;
