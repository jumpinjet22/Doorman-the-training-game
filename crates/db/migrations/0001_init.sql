-- First cut of the schema from KNOCK's plans/knock-game.md, section 6.
-- Deliberately no IP addresses or anything else about a player's device.

CREATE TABLE players (
    id          uuid PRIMARY KEY,
    created_at  timestamptz NOT NULL DEFAULT now(),
    handle      text NULL,
    email       text NULL,
    trust       real NOT NULL DEFAULT 0.1,
    flags       jsonb NOT NULL DEFAULT '{}'
);

CREATE TABLE items (
    id           uuid PRIMARY KEY,
    knock_key    text NOT NULL UNIQUE,
    mode         text NOT NULL CHECK (mode IN ('intent', 'pair')),
    -- The batch item as KNOCK exported it, plus `options` for intent items.
    -- Never sent to a browser as-is: it holds the gold answer and judge scores.
    payload      jsonb NOT NULL,
    gold_answer  text NULL,
    batch        text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE daily_sets (
    day           date PRIMARY KEY,
    item_ids      uuid[] NOT NULL,
    published_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE votes (
    id            bigserial PRIMARY KEY,
    player_id     uuid NOT NULL REFERENCES players (id) ON DELETE CASCADE,
    item_id       uuid NOT NULL REFERENCES items (id),
    day           date NULL,
    answer        text NOT NULL,
    ms_to_answer  integer NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    UNIQUE (player_id, item_id)
);
CREATE INDEX votes_item_id ON votes (item_id);

CREATE TABLE consensus (
    item_id       uuid PRIMARY KEY REFERENCES items (id),
    label         text NULL,
    share         real NOT NULL DEFAULT 0,
    total_weight  real NOT NULL DEFAULT 0,
    status        text NOT NULL CHECK (status IN ('open', 'settled', 'disputed')),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
