-- One row per player per day they played; `completed` once they've answered
-- every item in that day's set. Streaks are counted from these rows.

CREATE TABLE player_days (
    player_id  uuid NOT NULL REFERENCES players (id) ON DELETE CASCADE,
    day        date NOT NULL,
    completed  boolean NOT NULL DEFAULT false,
    PRIMARY KEY (player_id, day)
);

-- Days already played before this table existed.
INSERT INTO player_days (player_id, day, completed)
SELECT v.player_id, d.day, count(*) >= cardinality(d.item_ids)
FROM votes v
JOIN daily_sets d ON d.day = v.day AND v.item_id = ANY (d.item_ids)
GROUP BY v.player_id, d.day, d.item_ids;
