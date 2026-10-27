"""A real browser playing the real server."""

from __future__ import annotations

import re

import pytest
from axe_playwright_python.sync_playwright import Axe
from playwright.sync_api import Page, expect

from conftest import DAY, Dorman

pytestmark = pytest.mark.e2e


def _cards(page: Page):
    return page.locator(".card")


def test_play_a_full_day_and_resume_after_reload(page: Page, dorman: Dorman) -> None:
    errors: list[str] = []
    page.on("console", lambda msg: errors.append(msg.text) if msg.type == "error" else None)
    page.goto("/")
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


def test_each_browser_is_its_own_player(page: Page, dorman: Dorman, browser) -> None:
    page.goto("/")
    before = dorman.query("SELECT count(*) FROM players")[0][0]
    other = browser.new_context()
    other.new_page().goto(dorman.base_url + "/")
    other.close()
    assert dorman.query("SELECT count(*) FROM players")[0][0] == before + 1


def test_tomorrow_has_no_game_until_one_is_published(page: Page, dorman: Dorman) -> None:
    dorman.set_clock("2026-10-28T09:00:00Z")
    page.goto("/")
    expect(page.get_by_text("No game today")).to_be_visible()
    expect(_cards(page)).to_have_count(0)


def test_a_made_up_answer_is_refused(page: Page, dorman: Dorman) -> None:
    page.goto("/")
    item_id = _cards(page).first.get_attribute("data-item")
    response = page.request.post(
        "/api/vote", data={"item_id": item_id, "answer": "pizza_party", "ms_to_answer": 3000}
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
