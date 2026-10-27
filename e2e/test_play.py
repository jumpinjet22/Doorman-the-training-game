"""A real browser playing the real server."""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

import pytest
from axe_playwright_python.sync_playwright import Axe
from playwright.sync_api import Page, expect

from conftest import DAY, FIXTURE_BATCH, Dorman, FakeCloudflare, fake_turnstile, ingest

pytestmark = pytest.mark.e2e

# Records every CSP violation the browser reports, from before any page script runs.
CSP_LISTENER = """
window.__csp = [];
document.addEventListener("securitypolicyviolation", (e) =>
  window.__csp.push(`${e.violatedDirective} ${e.blockedURI}`));
"""

VIEWPORTS = {"desktop": (1280, 900), "phone": (360, 740)}


@pytest.fixture(params=list(VIEWPORTS))
def sized(request, page: Page) -> Page:
    """`page`, once at desktop size and once at phone size."""
    width, height = VIEWPORTS[request.param]
    page.set_viewport_size({"width": width, "height": height})
    return page


def _card(page: Page):
    return page.locator(".card")


def _answer(page: Page) -> None:
    """Answers the card on screen with its first choice and waits for the split."""
    card = _card(page)
    expect(page.locator(".next")).to_be_hidden()
    card.locator("button").first.click()
    expect(card.locator(".split")).to_contain_text("%")
    expect(page.locator(".next")).to_be_visible()


def _play_out(page: Page) -> int:
    """Answers every card left today; returns how many there were."""
    played = 0
    while _card(page).count():
        _answer(page)
        page.locator(".next").click()
        played += 1
    expect(page.get_by_role("heading", name="Today's results")).to_be_visible()
    return played


def _publish(dorman: Dorman, bin_dir: Path, day: str) -> None:
    """Publishes `day` unless an earlier test already did, from a fresh batch
    of its own: the example batch is too small to last many days, since gold
    retires after a few showings."""
    if dorman.query("SELECT 1 FROM daily_sets WHERE day = %s", day):
        return
    batch = json.loads(FIXTURE_BATCH.read_text(encoding="utf-8"))
    batch["batch"] = f"day-{day}"
    tag = day.replace("-", "")
    for item in batch["items"]:
        if item["mode"] == "pair":
            for reply in item["replies"]:
                reply["knock_key"] = f"{tag}{reply['knock_key']}"
            item["knock_key"] = ":".join(["pair", *(r["knock_key"] for r in item["replies"])])
            if item["gold_answer"]:
                item["gold_answer"] = f"{tag}{item['gold_answer']}"
        else:
            item["knock_key"] = f"{tag}{item['knock_key']}"
    path = Path(dorman.clock_file).parent / f"batch-{day}.json"
    path.write_text(json.dumps(batch), encoding="utf-8")
    ingest(bin_dir, dorman.db_url, "load", str(path))
    ingest(bin_dir, dorman.db_url, "approve", batch["batch"])
    ingest(bin_dir, dorman.db_url, "daily", "--day", day)


def _run_scorer(dorman: Dorman, today: str, out: Path) -> None:
    """The night run. One player is the whole crowd here, so the bar to settle
    is lowered until their answers are enough: gold is decided by the crowd
    like everything else, never settled on its known answer regardless. The
    bar is tiny because a browser test clicks in milliseconds, so its player's
    trust is flagged down several times over (timing is measured server-side)."""
    subprocess.run(
        [
            sys.executable,
            "-m",
            "dorman_scorer",
            "--database-url",
            dorman.db_url,
            "--out-dir",
            str(out),
            "--today",
            today,
            "--min-weight",
            "0.000001",
        ],
        check=True,
        capture_output=True,
    )


def _card_text(page: Page) -> str:
    return page.locator("#share-card").inner_text()


def test_play_a_full_day_resume_after_reload_and_copy_the_card(sized: Page, dorman: Dorman) -> None:
    page = sized
    errors: list[str] = []
    page.on("console", lambda msg: errors.append(msg.text) if msg.type == "error" else None)
    page.add_init_script(CSP_LISTENER)
    page.context.grant_permissions(["clipboard-read", "clipboard-write"], origin=dorman.base_url)
    response = page.goto("/")
    assert (
        "script-src 'self' https://challenges.cloudflare.com"
        in (response.headers["content-security-policy"])
    )
    expect(page.locator(".day")).to_have_text(f"Day 1 · {DAY}")
    expect(page.locator(".count")).to_have_text("Card 1 of 6")
    assert _card(page).count() == 1

    for _ in range(3):
        _answer(page)
        expect(_card(page).locator("button.mine")).to_have_count(1)
        page.locator(".next").click()
    # A reload picks up at the fourth card.
    page.reload()
    expect(page.locator(".count")).to_have_text("Card 4 of 6")
    expect(page.locator("progress.progress")).to_have_attribute("value", "4")

    assert _play_out(page) == 3
    card = _card_text(page)
    assert re.fullmatch(r"🚪 #1  [⬜🟩🟨🟥]{6}", card), card
    expect(page.locator(".result")).to_have_count(6)
    page.get_by_role("button", name="Copy").click()
    expect(page.locator("#copied")).to_have_text("Copied!")
    assert page.evaluate("navigator.clipboard.readText()") == card

    page.reload()
    expect(page.get_by_role("heading", name="Today's results")).to_be_visible()
    assert errors == []
    assert page.evaluate("window.__csp") == []


def test_a_day_can_be_played_with_plain_form_posts(page: Page, dorman: Dorman) -> None:
    """Without play.js each answer is a form post and a page load. (Cloudflare's
    own script still has to run: a vote needs its human check.)"""
    page.route("**/static/play.js", lambda route: route.abort())
    page.goto("/")
    for n in range(1, 7):
        expect(page.locator(".count")).to_have_text(f"Card {n} of 6")
        expect(page.locator('[name="cf-turnstile-response"]')).not_to_have_value("")
        _card(page).locator("button").first.click()
        expect(page).to_have_url(re.compile(r"/\?after="))
        expect(_card(page).locator(".split")).to_contain_text("%")
        expect(_card(page).locator("button.mine")).to_have_count(1)
        page.locator(".next").click()
    expect(page.get_by_role("heading", name="Today's results")).to_be_visible()
    # The Copy button needs the script; without it, it stays out of the way.
    expect(page.get_by_role("button", name="Copy")).to_be_hidden()


def test_the_next_morning_the_card_fills_in_and_the_streak_is_two(
    sized: Page, dorman: Dorman, bin_dir: Path, tmp_path: Path
) -> None:
    page = sized
    # Each viewport gets its own days: the night run is shared state.
    phone = page.viewport_size["width"] < 600
    first, second = ("2026-12-05", "2026-12-06") if phone else ("2026-12-01", "2026-12-02")
    number = 40 if phone else 36
    _publish(dorman, bin_dir, first)
    dorman.set_clock(f"{first}T12:00:00Z")
    page.goto("/")
    total = _play_out(page)
    assert _card_text(page) == f"🚪 #{number}  {'⬜' * total}"

    # The night run, then the next morning, before the next set goes up
    # (anything back in it would stay hidden). This checks the card fills in,
    # not settling thresholds, so the run's bar is low enough for one player.
    _run_scorer(dorman, second, tmp_path)
    dorman.set_clock(f"{second}T08:00:00Z")
    page.goto("/yesterday")
    filled = _card_text(page)
    assert re.fullmatch(rf"🚪 #{number}  [⬜🟩🟨🟥]{{{total}}}", filled), filled
    assert set(filled) & set("🟩🟨🟥"), "items are scored by morning"

    _publish(dorman, bin_dir, second)

    page.goto("/")
    _play_out(page)
    expect(page.locator("#share-card")).to_contain_text("🔥 2-day streak")


def test_skipping_a_day_resets_the_streak(sized: Page, dorman: Dorman, bin_dir: Path) -> None:
    page = sized
    for day in ("2026-12-11", "2026-12-13"):
        _publish(dorman, bin_dir, day)
        dorman.set_clock(f"{day}T12:00:00Z")
        page.goto("/")
        _play_out(page)
    expect(page.locator("#share-card")).not_to_contain_text("streak")
    assert _card_text(page).startswith("🚪 #48  ")


def _tab_to(page: Page, selector: str) -> None:
    for _ in range(40):
        page.keyboard.press("Tab")
        if page.evaluate(f"document.activeElement.matches({json.dumps(selector)})"):
            return
    raise AssertionError(f"couldn't tab to {selector}")


def test_a_whole_day_by_keyboard_alone(sized: Page, dorman: Dorman) -> None:
    page = sized
    page.context.grant_permissions(["clipboard-read", "clipboard-write"], origin=dorman.base_url)
    page.goto("/")
    for _ in range(6):
        _tab_to(page, ".card button")
        page.keyboard.press("Enter")
        expect(_card(page).locator(".split")).to_contain_text("%")
        # Focus moves to the way on.
        expect(page.locator(".next")).to_be_focused()
        with page.expect_navigation():
            page.keyboard.press("Enter")
    expect(page.get_by_role("heading", name="Today's results")).to_be_visible()
    _tab_to(page, "#copy")
    page.keyboard.press("Enter")
    expect(page.locator("#copied")).to_have_text("Copied!")


def test_each_browser_is_its_own_player(page: Page, dorman: Dorman, browser) -> None:
    before = dorman.query("SELECT count(*) FROM players")[0][0]
    page.goto("/")
    assert dorman.query("SELECT count(*) FROM players")[0][0] == before, "looking adds no one"
    _answer(page)
    other = browser.new_context()
    fake_turnstile(other)
    other_page = other.new_page()
    other_page.goto(dorman.base_url + "/")
    _answer(other_page)
    other.close()
    assert dorman.query("SELECT count(*) FROM players")[0][0] == before + 2


def test_tomorrow_has_no_game_until_one_is_published(page: Page, dorman: Dorman) -> None:
    dorman.set_clock("2026-10-28T09:00:00Z")
    page.goto("/")
    expect(page.get_by_text("No game today")).to_be_visible()
    expect(page.locator("#countdown")).to_have_text("15 h 0 min")
    expect(_card(page)).to_have_count(0)


def test_a_made_up_answer_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    item_id = _card(page).get_attribute("data-item")
    response = page.request.post(
        "/api/vote",
        data={"item_id": item_id, "answer": "pizza_party", "ms_to_answer": 3000},
        headers={"origin": dorman.base_url},
    )
    assert response.status == 422


def test_phone_width_has_no_sideways_scroll(page: Page) -> None:
    page.set_viewport_size({"width": 360, "height": 740})
    for path in ("/", "/yesterday", "/about"):
        page.goto(path)
        overflow = page.evaluate("document.documentElement.scrollWidth - window.innerWidth")
        assert overflow <= 0, path


def test_the_pages_leak_no_answers(page: Page) -> None:
    for path in ("/", "/yesterday", "/about"):
        page.goto(path)
        html = page.content()
        for field in ("gold", "generator", "judge", "knock_key", "category_correct"):
            assert field not in html, f"{path} mentions {field}"
        assert not re.search(r"\bexample-1\b", html)


def _serious_violations(page: Page) -> list[str]:
    violations = Axe().run(page).response["violations"]
    return [f"{v['id']}: {v['help']}" for v in violations if v["impact"] in ("serious", "critical")]


def test_no_serious_accessibility_violations_on_any_page(sized: Page, dorman: Dorman) -> None:
    page = sized
    checked: dict[str, list[str]] = {}

    def check(name: str) -> None:
        checked[name] = _serious_violations(page)

    page.goto("/")
    check("card")
    _answer(page)
    check("answered card")
    page.goto("/about")
    check("about")
    page.goto("/yesterday")
    check("yesterday, no game")
    page.goto("/")
    _play_out(page)
    check("results")
    page.goto("/overtime")
    check("overtime, nothing left")
    page.goto("/leaderboard")
    check("leaderboard")
    dorman.set_clock("2026-10-28T09:00:00Z")
    page.goto("/")
    check("no game today")
    page.goto("/yesterday")
    check("yesterday")
    assert {name: found for name, found in checked.items() if found} == {}


def test_a_vote_without_a_human_check_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    card = _card(page)
    vote = {
        "item_id": card.get_attribute("data-item"),
        "answer": card.locator("button").first.get_attribute("value"),
        "ms_to_answer": 3000,
    }
    origin = {"origin": dorman.base_url}
    for token in (None, "forged", "XXXX.DUMMY.TOKEN.XXXX"):
        data = vote if token is None else {**vote, "turnstile_token": token}
        assert page.request.post("/api/vote", data=data, headers=origin).status == 403


def test_a_cross_origin_post_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    item_id = _card(page).get_attribute("data-item")
    data = {"item_id": item_id, "answer": "x", "ms_to_answer": 1, "turnstile_token": "e2e-pass-x"}
    for headers in ({}, {"origin": "https://evil.example"}):
        assert page.request.post("/api/vote", data=data, headers=headers).status == 403
    form = {"item_id": item_id, "answer": "x", "shown_at": "0"}
    evil = {"origin": "https://evil.example"}
    assert page.request.post("/vote", form=form, headers=evil).status == 403


def test_a_cloudflare_outage_blocks_votes_until_it_recovers(
    page: Page, dorman: Dorman, cloudflare: FakeCloudflare
) -> None:
    page.goto("/")
    card = _card(page)
    item_id = card.get_attribute("data-item")
    before = dorman.query("SELECT count(*) FROM votes WHERE item_id = %s", item_id)[0][0]

    cloudflare.down = True
    card.locator("button").first.click()
    expect(card.locator(".split")).to_have_text(
        "Couldn't confirm you're human just now. Try again."
    )
    expect(card.locator("button").first).to_be_enabled()
    expect(card.locator("button").first).to_be_focused()
    assert dorman.query("SELECT count(*) FROM votes WHERE item_id = %s", item_id)[0][0] == before

    cloudflare.down = False
    card.locator("button").first.click()
    expect(card.locator(".split")).to_contain_text("%")
    assert dorman.query("SELECT count(*) FROM votes WHERE item_id = %s", item_id)[0][0] == (
        before + 1
    )


def test_a_batch_is_played_only_once_reviewed_and_its_markup_stays_text(
    page: Page, dorman: Dorman, bin_dir: Path, tmp_path: Path
) -> None:
    """Markup in a visitor's line shows as text, and runs nothing."""
    batch = json.loads(FIXTURE_BATCH.read_text(encoding="utf-8"))
    batch["batch"] = "markup"
    items = [i for i in batch["items"] if i["mode"] == "intent" and not i["gold_answer"]][:2]
    for n, item in enumerate(items):
        item["knock_key"] = f"markup-{n}"
        item["text"] = "<script>alert(1)</script>"
        item["scene"] = '<img src=x onerror="alert(2)">'
    batch["items"] = items
    path = tmp_path / "markup.json"
    path.write_text(json.dumps(batch), encoding="utf-8")

    def keys_on(day: str) -> set[str]:
        rows = dorman.query(
            "SELECT i.knock_key FROM daily_sets d JOIN items i ON i.id = ANY (d.item_ids)"
            " WHERE d.day = %s",
            day,
        )
        return {key for (key,) in rows}

    assert "pending review" in ingest(bin_dir, dorman.db_url, "load", str(path))
    review = ingest(bin_dir, dorman.db_url, "review", "markup")
    assert "2 items, pending" in review
    assert review.count("<script>alert(1)</script>") == 2
    ingest(bin_dir, dorman.db_url, "daily", "--day", "2026-11-01")
    assert not any(key.startswith("markup-") for key in keys_on("2026-11-01"))

    ingest(bin_dir, dorman.db_url, "approve", "markup")
    ingest(bin_dir, dorman.db_url, "daily", "--day", "2026-11-02")
    assert {"markup-0", "markup-1"} <= keys_on("2026-11-02")

    dialogs: list[str] = []
    page.on("dialog", lambda d: (dialogs.append(d.message), d.dismiss()))
    page.add_init_script(CSP_LISTENER)
    dorman.set_clock("2026-11-02T12:00:00Z")
    page.goto("/")
    _play_out(page)
    # The results list every card, the markup ones included, as text.
    expect(page.get_by_text("<script>alert(1)</script>")).to_have_count(2)
    expect(page.get_by_text('<img src=x onerror="alert(2)">')).to_have_count(2)
    assert page.locator("main script, main img").count() == 0
    assert dialogs == []
    assert page.evaluate("window.__csp") == []


def test_from_the_daily_set_into_overtime_and_back(
    sized: Page, dorman: Dorman, bin_dir: Path
) -> None:
    page = sized
    phone = page.viewport_size["width"] < 600
    earlier, today = ("2027-01-20", "2027-01-21") if phone else ("2027-01-10", "2027-01-11")
    _publish(dorman, bin_dir, earlier)
    _publish(dorman, bin_dir, today)
    dorman.set_clock(f"{today}T12:00:00Z")
    page.goto("/overtime")
    expect(page).to_have_url(re.compile(r"/$"), timeout=5000)  # finish today first
    _play_out(page)

    untimed = "SELECT count(*) FROM votes WHERE day IS NULL"
    before = dorman.query(untimed)[0][0]
    page.get_by_role("link", name="Play Overtime").click()
    expect(page.get_by_role("heading", name="Overtime")).to_be_visible()
    assert _serious_violations(page) == []
    seen = []
    for _ in range(2):
        seen.append(_card(page).get_attribute("data-item"))
        _answer(page)
        expect(page.locator(".next")).to_have_attribute("href", "/overtime")
        page.locator(".next").click()
    assert len(set(seen)) == 2

    def shown(where: str) -> set[str]:
        sql = f"SELECT DISTINCT unnest(item_ids)::text FROM daily_sets WHERE {where}"
        return {item for (item,) in dorman.query(sql, today)}

    assert set(seen) <= shown("day < %s"), "Overtime serves items from earlier days"
    assert not set(seen) & shown("day = %s"), "and never today's"
    expect(page.locator(".count")).to_contain_text("Overtime answers so far: 2")

    page.get_by_role("link", name="Back to today's results").click()
    expect(page.get_by_role("heading", name="Today's results")).to_be_visible()
    assert dorman.query(untimed)[0][0] == before + 2, "Overtime answers belong to no day"
