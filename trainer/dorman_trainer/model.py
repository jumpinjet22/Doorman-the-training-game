"""The baseline: TF-IDF over words and characters, then logistic regression.
Trains in seconds on a CPU, and is the number every later model has to beat."""

from __future__ import annotations

from sklearn.feature_extraction.text import TfidfVectorizer
from sklearn.linear_model import LogisticRegression
from sklearn.metrics import f1_score, precision_recall_fscore_support
from sklearn.pipeline import Pipeline, make_union

from dorman_trainer.data import Example


def train(examples: list[Example]) -> Pipeline:
    pipeline = Pipeline(
        [
            (
                "features",
                make_union(
                    TfidfVectorizer(ngram_range=(1, 2), sublinear_tf=True),
                    # Character n-grams cope with typos and odd spellings.
                    TfidfVectorizer(analyzer="char_wb", ngram_range=(2, 5), sublinear_tf=True),
                ),
            ),
            ("classify", LogisticRegression(max_iter=2000, class_weight="balanced")),
        ]
    )
    pipeline.fit(
        [e.text for e in examples],
        [e.label for e in examples],
        classify__sample_weight=[e.weight for e in examples],
    )
    return pipeline


def evaluate(model: Pipeline, examples: list[Example]) -> dict:
    """Macro-F1 over the labels in `examples`, and precision and recall per
    label, on unweighted rows: every test item counts the same."""
    truth = [e.label for e in examples]
    predicted = list(model.predict([e.text for e in examples]))
    labels = sorted(set(truth))
    precision, recall, _, support = precision_recall_fscore_support(
        truth, predicted, labels=labels, zero_division=0
    )
    return {
        "macro_f1": float(
            f1_score(truth, predicted, labels=labels, average="macro", zero_division=0)
        ),
        "per_label": {
            label: {"precision": float(p), "recall": float(r), "support": int(n)}
            for label, p, r, n in zip(labels, precision, recall, support, strict=True)
        },
    }
