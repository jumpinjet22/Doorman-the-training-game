// Vote buttons on the play page: get a Turnstile token, post the answer,
// then show the crowd's split.

// One Turnstile widget, asked for a fresh single-use token before each vote.
// Requests queue up so two quick clicks don't race for the same widget.
const turnstileBox = document.getElementById("turnstile");
let widget = null;
let waiting = null;
let queue = Promise.resolve();

function humanCheck() {
  if (!window.turnstile) return Promise.reject(new Error("Turnstile didn't load"));
  if (widget === null) {
    widget = turnstile.render(turnstileBox, {
      sitekey: turnstileBox.dataset.sitekey,
      action: "vote",
      execution: "execute",
      appearance: "interaction-only",
      callback: (token) => waiting?.resolve(token),
      "error-callback": () => { waiting?.reject(new Error("Turnstile failed")); return true; },
      "expired-callback": () => turnstile.reset(widget),
    });
  }
  const token = new Promise((resolve, reject) => { waiting = { resolve, reject }; });
  turnstile.reset(widget);
  turnstile.execute(turnstileBox);
  return token;
}

const MESSAGES = {
  403: "Couldn't confirm you're human. Try again.",
  429: "Slow down a little, then try again.",
  503: "Couldn't confirm you're human just now. Try again.",
};

let shownAt = performance.now();
document.querySelectorAll(".card").forEach((card) => {
  const buttons = card.querySelectorAll("button");
  const box = card.querySelector(".split");
  const fail = (button, message) => {
    box.textContent = message;
    buttons.forEach((b) => (b.disabled = false));
    button.classList.remove("mine");
  };
  buttons.forEach((button) => {
    button.addEventListener("click", () => {
      buttons.forEach((b) => (b.disabled = true));
      button.classList.add("mine");
      const ms = Math.round(performance.now() - shownAt);
      shownAt = performance.now();
      queue = queue.then(async () => {
        let res;
        try {
          res = await fetch("/api/vote", {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              item_id: card.dataset.item,
              answer: button.dataset.answer,
              ms_to_answer: ms,
              turnstile_token: await humanCheck(),
            }),
          });
        } catch {
          fail(button, MESSAGES[503]);
          return;
        }
        if (!res.ok) { fail(button, MESSAGES[res.status] ?? "Couldn't save that one."); return; }
        const { split } = await res.json();
        const total = split.reduce((n, row) => n + row.count, 0);
        box.replaceChildren(...split.flatMap((row) => {
          const label = document.createElement("div");
          const pct = Math.round((100 * row.count) / total);
          label.textContent = `${row.label}: ${pct}%`;
          const bar = document.createElement("div");
          bar.className = "bar";
          bar.style.width = `${pct}%`;
          return [label, bar];
        }));
      });
    });
  });
});
