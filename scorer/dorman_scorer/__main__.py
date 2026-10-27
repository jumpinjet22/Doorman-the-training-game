"""dorman-scorer: the nightly cron job.

    DATABASE_URL=postgres://... python -m dorman_scorer

Recomputes every player's trust and every item's verdict, saves both, and
logs the run in scoring_runs. The admin dashboard builds datasets from what
is saved.

The whole run is one transaction: if it fails part way, nothing it wrote is
kept, and running it again for the same day replaces the earlier run.
Thresholds come from --profile, then DORMAN_* environment variables, then
the flags below, each overriding the one before.
"""

from __future__ import annotations

import argparse
import logging
import os
from collections.abc import Mapping
from datetime import date

import psycopg

from dorman_scorer import config as cfg
from dorman_scorer import db
from dorman_scorer.scoring import score_all

log = logging.getLogger("dorman_scorer")

_HELP = {
    "min_weight": "total trust an item needs before it can settle",
    "min_share": "weighted share that settles an item on its own",
    "min_posterior": "Dawid-Skene probability that settles an item...",
    "min_posterior_share": "...when its weighted share is at least this",
    "dispute_after_days": "days after first showing before an unsettled item is disputed",
    "new_account_hours": "accounts younger than this when voting count as new",
    "brigade_min_votes": "new-account votes for one answer on one item that make a brigade",
    "cohort_window_hours": "accounts created this close together can form a cohort",
    "cohort_min_shared": "items two accounts must both answer before they can be linked",
    "cohort_agreement": "share of those answers that must match to link two accounts",
    "cohort_min_size": "linked accounts that make a cohort",
}


def config_from(args: argparse.Namespace, environ: Mapping[str, str]) -> cfg.Config:
    if args.profile not in cfg.PROFILES:
        raise ValueError(f"unknown profile {args.profile!r}")
    base = cfg.PROFILES[args.profile]
    overrides: dict[str, float | int] = {}
    for name, var in cfg.ENV.items():
        kind = type(getattr(base, name))
        if var in environ:
            overrides[name] = kind(environ[var])
        if getattr(args, name) is not None:
            overrides[name] = kind(getattr(args, name))
    return cfg.build(args.profile, **overrides)


def warnings_for(config: cfg.Config) -> list[str]:
    """Settings that switch a defence off, in words for the run log."""
    found = []
    if config.new_account_hours <= 0:
        found.append(
            "new_account_hours is 0: brigade detection by account age is off "
            "(the soft-launch profile). Switch back to the default profile after launch."
        )
    return found


def main(argv: list[str] | None = None, environ: Mapping[str, str] | None = None) -> int:
    logging.basicConfig(level=logging.INFO, format="%(levelname)s %(name)s: %(message)s")
    environ = os.environ if environ is None else environ
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--database-url", default=environ.get("DATABASE_URL"))
    parser.add_argument(
        "--today",
        type=date.fromisoformat,
        default=None,
        help="score as of this day (default: today in UTC, per the database)",
    )
    parser.add_argument(
        "--profile",
        choices=sorted(cfg.PROFILES),
        default=environ.get("DORMAN_SCORER_PROFILE", "default"),
        help="threshold preset (env DORMAN_SCORER_PROFILE)",
    )
    for name, var in cfg.ENV.items():
        parser.add_argument(
            "--" + name.replace("_", "-"),
            type=type(getattr(cfg.Config(), name)),
            default=None,
            help=f"{_HELP[name]} (env {var}; default {getattr(cfg.Config(), name)})",
        )
    args = parser.parse_args(argv)
    if not args.database_url:
        parser.error("--database-url or DATABASE_URL is required")
    try:
        config = config_from(args, environ)
    except ValueError as err:
        parser.error(f"bad configuration: {err}")

    warnings = warnings_for(config)
    for warning in warnings:
        log.warning("%s", warning)

    with psycopg.connect(args.database_url) as conn, conn.transaction():
        db.lock(conn)
        items, votes = db.load(conn)
        today = args.today or db.today(conn)
        players, verdicts, gold_rows = score_all(items, votes, today, config)
        decided = [v for v in verdicts if v.status != "open"]
        # Gold gets rows too, decided by the crowd the same way, so the reveal
        # page can't tell it apart; only the non-gold verdicts are counted.
        db.save(conn, players, [*verdicts, *gold_rows])
        settled = sum(1 for v in decided if v.status == "settled")
        db.record_run(
            conn,
            db.Run(
                day=today,
                players=len(players),
                items=len(verdicts),
                settled=settled,
                disputed=len(decided) - settled,
                brigaded=sum(1 for v in verdicts if v.zeroed_votes),
                config=config.as_dict(),
                warnings=warnings,
            ),
        )

    print(
        f"Scored {len(players)} players and {len(verdicts)} items: {settled} settled, "
        f"{len(decided) - settled} disputed."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
