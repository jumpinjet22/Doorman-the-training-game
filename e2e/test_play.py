"""A real browser playing the real server."""

from __future__ import annotations

import json
import re
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


def _cards(page: Page):
    return page.locator(".card")


def _vote_on_first_card(page: Page) -> None:
    card = _cards(page).first
    card.locator("button").first.click()
    expect(card.locator(".split")).to_contain_text("%")


def test_play_a_full_day_and_resume_after_reload(page: Page, dorman: Dorman) -> None:
    errors: list[str] = []
    page.on("console", lambda msg: errors.append(msg.text) if msg.type == "error" else None)
    page.add_init_script(CSP_LISTENER)
    response = page.goto("/")
    csp = response.headers["content-security-policy"]
    assert "script-src 'self' https://challenges.cloudflare.com" in csp
    expect(page.locator(".day")).to_have_text(DAY)
    cards = _cards(page)
    total = cards.count()
    assert total == 6

    for i in range(total):
        card = cards.nth(i)
        card.locator("button").first.click()
        expect(card.locator(".split")).to_contain_text("%")
        expect(card.locator("button.mine")).to_have_count(1)

    page.reload()
    for i in range(total):
        card = cards.nth(i)
        expect(card.locator("button.mine")).to_have_count(1)
        for button in card.locator("button").all():
            expect(button).to_be_disabled()
    assert errors == []
    assert page.evaluate("window.__csp") == []


def test_each_browser_is_its_own_player(page: Page, dorman: Dorman, browser) -> None:
    before = dorman.query("SELECT count(*) FROM players")[0][0]
    page.goto("/")
    assert dorman.query("SELECT count(*) FROM players")[0][0] == before, "looking adds no one"
    _vote_on_first_card(page)
    other = browser.new_context()
    fake_turnstile(other)
    other_page = other.new_page()
    other_page.goto(dorman.base_url + "/")
    _vote_on_first_card(other_page)
    other.close()
    assert dorman.query("SELECT count(*) FROM players")[0][0] == before + 2


def test_tomorrow_has_no_game_until_one_is_published(page: Page, dorman: Dorman) -> None:
    dorman.set_clock("2026-10-28T09:00:00Z")
    page.goto("/")
    expect(page.get_by_text("No game today")).to_be_visible()
    expect(_cards(page)).to_have_count(0)


def test_a_made_up_answer_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    item_id = _cards(page).first.get_attribute("data-item")
    response = page.request.post(
        "/api/vote",
        data={"item_id": item_id, "answer": "pizza_party", "ms_to_answer": 3000},
        headers={"origin": dorman.base_url},
    )
    assert response.status == 422


def test_phone_width_has_no_sideways_scroll(page: Page) -> None:
    page.set_viewport_size({"width": 360, "height": 740})
    page.goto("/")
    overflow = page.evaluate("document.documentElement.scrollWidth - window.innerWidth")
    assert overflow <= 0


def test_the_page_leaks_no_answers(page: Page) -> None:
    page.goto("/")
    html = page.content()
    for field in ("gold", "generator", "judge", "knock_key", "category_correct"):
        assert field not in html
    assert not re.search(r"\bexample-1\b", html)


@pytest.mark.parametrize("width", [1280, 360])
def test_no_serious_accessibility_violations(page: Page, width: int) -> None:
    page.set_viewport_size({"width": width, "height": 900})
    page.goto("/")
    violations = Axe().run(page).response["violations"]
    serious = [
        f"{v['id']}: {v['help']}" for v in violations if v["impact"] in ("serious", "critical")
    ]
    assert serious == []


def test_a_vote_without_a_human_check_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    card = _cards(page).first
    vote = {
        "item_id": card.get_attribute("data-item"),
        "answer": card.locator("button").first.get_attribute("data-answer"),
        "ms_to_answer": 3000,
    }
    origin = {"origin": dorman.base_url}
    for token in (None, "forged", "XXXX.DUMMY.TOKEN.XXXX"):
        data = vote if token is None else {**vote, "turnstile_token": token}
        assert page.request.post("/api/vote", data=data, headers=origin).status == 403


def test_a_cross_origin_post_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    item_id = _cards(page).first.get_attribute("data-item")
    data = {"item_id": item_id, "answer": "x", "ms_to_answer": 1, "turnstile_token": "e2e-pass-x"}
    for headers in ({}, {"origin": "https://evil.example"}):
        assert page.request.post("/api/vote", data=data, headers=headers).status == 403


def test_a_cloudflare_outage_blocks_votes_until_it_recovers(
    page: Page, dorman: Dorman, cloudflare: FakeCloudflare
) -> None:
    page.goto("/")
    card = _cards(page).nth(1)
    item_id = card.get_attribute("data-item")
    before = dorman.query("SELECT count(*) FROM votes WHERE item_id = %s", item_id)[0][0]

    cloudflare.down = True
    card.locator("button").first.click()
    expect(card.locator(".split")).to_have_text(
        "Couldn't confirm you're human just now. Try again."
    )
    expect(card.locator("button").first).to_be_enabled()
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
    expect(page.get_by_text("<script>alert(1)</script>").first).to_be_visible()
    expect(page.get_by_text('<img src=x onerror="alert(2)">').first).to_be_visible()
    assert page.locator("main script, main img").count() == 0
    assert dialogs == []
    assert page.evaluate("window.__csp") == []
