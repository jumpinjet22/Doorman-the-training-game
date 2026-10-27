-- How sure the player said they were. Optional, feedback for the player only:
-- the scorer ignores it.
ALTER TABLE votes ADD COLUMN confidence text
    CHECK (confidence IN ('unsure', 'fairly', 'certain'));
