// Makes the pages nicer with JavaScript; every page works without it.
// - A card's form posts by fetch, so the crowd's split shows without a reload.
// - The no-game page counts down to the next game.
// - The results page can copy or share the card.

const shownAt = performance.now();

// Cloudflare's widget puts a single-use token in the form once it's sure
// there's a person here. Wait for one we haven't spent yet.
let spent = "";
function freshToken(form) {
  const field = () => form.querySelector('[name="cf-turnstile-response"]');
  return new Promise((resolve, reject) => {
    const started = Date.now();
    const poll = () => {
      const token = field()?.value;
      if (token && token !== spent) { spent = token; resolve(token); return; }
      if (Date.now() - started > 15000) { reject(new Error("no human check")); return; }
      setTimeout(poll, 100);
    };
    poll();
  });
}

const MESSAGES = {
  403: "Couldn't confirm you're human. Try again.",
  429: "Slow down a little, then try again.",
  503: "Couldn't confirm you're human just now. Try again.",
};

function renderBars(box, split, mine) {
  const total = split.reduce((n, row) => n + row.count, 0);
  const list = document.createElement("ul");
  list.className = "bars";
  for (const row of split) {
    const pct = Math.round((100 * row.count) / total);
    const item = document.createElement("li");
    if (row.answer === mine) item.className = "mine";
    const label = document.createElement("span");
    label.className = "label";
    label.textContent = row.label + (row.answer === mine ? " (you)" : "");
    const share = document.createElement("span");
    share.className = "pct";
    share.textContent = `${pct}%`;
    const meter = document.createElement("meter");
    Object.assign(meter, { min: 0, max: 100, value: pct });
    meter.setAttribute("aria-hidden", "true");
    item.append(label, " ", share, meter);
    list.append(item);
  }
  box.replaceChildren(list);
}

document.querySelectorAll("form.vote").forEach((form) => {
  const card = form.closest(".card");
  const box = card.querySelector(".split");
  const next = card.querySelector(".next");
  const buttons = form.querySelectorAll("button");
  form.addEventListener("submit", async (event) => {
    event.preventDefault();
    const button = event.submitter;
    buttons.forEach((b) => (b.disabled = true));
    button.classList.add("mine");
    box.textContent = "Saving…";
    const fail = (message) => {
      box.textContent = message;
      buttons.forEach((b) => (b.disabled = false));
      button.classList.remove("mine");
      button.focus();
      window.turnstile?.reset();
    };
    let res;
    try {
      res = await fetch("/api/vote", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          item_id: form.elements.item_id.value,
          answer: button.value,
          ms_to_answer: Math.round(performance.now() - shownAt),
          turnstile_token: await freshToken(form),
        }),
      });
    } catch {
      fail(MESSAGES[503]);
      return;
    }
    if (!res.ok) { fail(MESSAGES[res.status] ?? "Couldn't save that one."); return; }
    const { remaining, split } = await res.json();
    renderBars(box, split, button.value);
    next.textContent = remaining === 0 ? "See your results" : "Next card";
    next.hidden = false;
    next.focus();
  });
});

const countdown = document.getElementById("countdown");
if (countdown) {
  const reset = Date.parse(countdown.getAttribute("datetime"));
  const tick = () => {
    const minutes = Math.ceil(Math.max(0, reset - Date.now()) / 60000);
    if (minutes === 0) { location.reload(); return; }
    const hours = Math.floor(minutes / 60);
    countdown.textContent = hours ? `${hours} h ${minutes % 60} min` : `${minutes} min`;
  };
  setInterval(tick, 15000);
}

const card = document.getElementById("share-card");
const copy = document.getElementById("copy");
if (card && copy) {
  const status = document.getElementById("copied");
  copy.hidden = false;
  copy.addEventListener("click", async () => {
    try {
      await navigator.clipboard.writeText(card.textContent);
      status.textContent = "Copied!";
    } catch {
      status.textContent = "Couldn't copy; select the card and copy it yourself.";
    }
  });
  const share = document.getElementById("share");
  if (navigator.share) {
    share.hidden = false;
    share.addEventListener("click", () => navigator.share({ text: card.textContent }).catch(() => {}));
  }
}
