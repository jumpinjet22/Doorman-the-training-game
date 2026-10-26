"""dorman-scorer: the nightly cron job.

    DATABASE_URL=postgres://... python -m dorman_scorer --out-dir exports/

Recomputes every player's trust and every item's verdict, saves both, and
writes consensus-<date>.json (settled and disputed items only) for KNOCK's
scripts/import_game_consensus.py.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path

import psycopg

from dorman_scorer import db
from dorman_scorer.consensus import consensus_file
from dorman_scorer.scoring import score


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--database-url", default=os.environ.get("DATABASE_URL"))
    parser.add_argument("--out-dir", type=Path, default=Path("."))
    args = parser.parse_args()
    if not args.database_url:
        parser.error("--database-url or DATABASE_URL is required")

    with psycopg.connect(args.database_url) as conn:
        items, votes = db.load(conn)
        today = db.today(conn)
        players, verdicts = score(items, votes, today)
        db.save(conn, players, verdicts)

    decided = [v for v in verdicts if v.status != "open"]
    args.out_dir.mkdir(parents=True, exist_ok=True)
    out = args.out_dir / f"consensus-{today.isoformat()}.json"
    out.write_text(json.dumps(consensus_file(today, decided), indent=2) + "\n", encoding="utf-8")
    settled = sum(1 for v in decided if v.status == "settled")
    print(
        f"Scored {len(players)} players and {len(verdicts)} items: {settled} settled, "
        f"{len(decided) - settled} disputed. Wrote {out}."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
