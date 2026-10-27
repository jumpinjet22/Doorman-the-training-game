"""Weighted Dawid-Skene: jointly estimate each item's true label and each
player's confusion matrix from noisy votes.

Gold items are clamped to their known answer, so they anchor the confusion
matrices instead of being re-estimated.
"""

from __future__ import annotations

from dataclasses import dataclass

import numpy as np

# Dirichlet smoothing on confusion-matrix rows: diagonal gets a little more,
# so a player with few votes is assumed mildly competent, not random.
_SMOOTH_DIAG = 2.0
_SMOOTH_OFF = 0.5


@dataclass
class DawidSkeneResult:
    labels: list[str]
    # item id -> probability over `labels`
    posteriors: dict[str, np.ndarray]
    # player id -> expected accuracy under the estimated class priors
    accuracy: dict[str, float]


def dawid_skene(
    votes: list[tuple[str, str, str]],
    labels: list[str],
    *,
    init_weights: dict[str, float] | None = None,
    gold: dict[str, str] | None = None,
    iterations: int = 50,
    tol: float = 1e-6,
) -> DawidSkeneResult:
    """`votes` are (player_id, item_id, label) triples; labels not in
    `labels` are ignored. `init_weights` seeds the first label estimate
    (e.g. gold trust) instead of a plain majority vote.
    """
    index = {label: i for i, label in enumerate(labels)}
    votes = [(p, it, lab) for p, it, lab in votes if lab in index]
    gold = {k: v for k, v in (gold or {}).items() if v in index}
    items = sorted({it for _, it, _ in votes})
    players = sorted({p for p, _, _ in votes})
    k = len(labels)
    if not items or k == 0:
        return DawidSkeneResult(labels, {}, {})

    item_ix = {it: i for i, it in enumerate(items)}
    player_ix = {p: i for i, p in enumerate(players)}
    v_item = np.array([item_ix[it] for _, it, _ in votes])
    v_player = np.array([player_ix[p] for p, _, _ in votes])
    v_label = np.array([index[lab] for _, _, lab in votes])
    weights = init_weights or {}
    v_weight = np.array([weights.get(p, 1.0) for p, _, _ in votes])

    def clamp(t: np.ndarray) -> np.ndarray:
        for it, answer in gold.items():
            if it in item_ix:
                t[item_ix[it]] = 0.0
                t[item_ix[it], index[answer]] = 1.0
        return t

    # Initial estimate: weighted vote share per item.
    t = np.full((len(items), k), 1e-9)
    np.add.at(t, (v_item, v_label), v_weight)
    t = clamp(t / t.sum(axis=1, keepdims=True))

    smooth = np.full((k, k), _SMOOTH_OFF) + np.eye(k) * (_SMOOTH_DIAG - _SMOOTH_OFF)
    for _ in range(iterations):
        # M-step: class priors and per-player confusion matrices.
        prior = t.mean(axis=0) + 1e-9
        prior /= prior.sum()
        conf = np.broadcast_to(smooth, (len(players), k, k)).copy()
        np.add.at(conf, (v_player, slice(None), v_label), t[v_item])
        conf /= conf.sum(axis=2, keepdims=True)

        # E-step: posterior over each item's true label.
        log_t = np.tile(np.log(prior), (len(items), 1))
        np.add.at(log_t, v_item, np.log(conf[v_player, :, v_label]))
        log_t -= log_t.max(axis=1, keepdims=True)
        new_t = np.exp(log_t)
        new_t = clamp(new_t / new_t.sum(axis=1, keepdims=True))
        done = np.abs(new_t - t).max() < tol
        t = new_t
        if done:
            break

    accuracy = {p: float(np.sum(prior * np.diag(conf[player_ix[p]]))) for p in players}
    return DawidSkeneResult(labels, {it: t[item_ix[it]] for it in items}, accuracy)
