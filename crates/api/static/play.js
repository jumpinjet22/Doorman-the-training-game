// Vote buttons on the play page: post the answer, then show the crowd's split.
let shownAt = performance.now();
document.querySelectorAll(".card").forEach((card) => {
  card.querySelectorAll("button").forEach((button) => {
    button.addEventListener("click", async () => {
      const buttons = card.querySelectorAll("button");
      buttons.forEach((b) => (b.disabled = true));
      button.classList.add("mine");
      const res = await fetch("/api/vote", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          item_id: card.dataset.item,
          answer: button.dataset.answer,
          ms_to_answer: Math.round(performance.now() - shownAt),
        }),
      });
      shownAt = performance.now();
      if (!res.ok) { card.querySelector(".split").textContent = "Couldn't save that one."; return; }
      const { split } = await res.json();
      const total = split.reduce((n, row) => n + row.count, 0);
      const box = card.querySelector(".split");
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
