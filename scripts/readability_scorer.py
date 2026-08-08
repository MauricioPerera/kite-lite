"""Text readability scorer: computes the classic Flesch Reading Ease and
Flesch-Kincaid Grade Level scores over a page's extracted text.

Honest limitation: this is the standard ENGLISH-calibrated formula
(average syllables-per-word and words-per-sentence, using an English
syllable-counting heuristic — vowel groups, adjusted for a silent
trailing 'e'). Running it on Spanish or other languages will produce a
number, but not a meaningful one — syllable patterns differ too much.

Usage:
    python scripts/readability_scorer.py <url|page.json>

Env override: KITE_LITE_BIN (path to the kite-lite binary).
"""

import json
import re
import sys
from pathlib import Path

from _kite_lite import fetch_page


def load_page(target):
    if target.startswith("http://") or target.startswith("https://"):
        return fetch_page(target)
    return json.loads(Path(target).read_text(encoding="utf-8"))


def count_syllables(word):
    word = word.lower().strip(".,!?;:\"'()")
    if not word:
        return 0
    groups = re.findall(r"[aeiouy]+", word)
    count = len(groups)
    if word.endswith("e") and count > 1:
        count -= 1
    return max(count, 1)


def analyze(text):
    sentences = [s for s in re.split(r"[.!?]+", text) if s.strip()]
    words = re.findall(r"[A-Za-z']+", text)
    syllables = sum(count_syllables(w) for w in words)
    return len(sentences), len(words), syllables


def compute_scores(sentence_count, word_count, syllable_count):
    if sentence_count == 0 or word_count == 0:
        return None
    words_per_sentence = word_count / sentence_count
    syllables_per_word = syllable_count / word_count
    reading_ease = 206.835 - 1.015 * words_per_sentence - 84.6 * syllables_per_word
    grade_level = 0.39 * words_per_sentence + 11.8 * syllables_per_word - 15.59
    return reading_ease, grade_level, words_per_sentence, syllables_per_word


def interpret(ease):
    if ease >= 90:
        return "muy facil (5to grado)"
    if ease >= 70:
        return "facil"
    if ease >= 60:
        return "estandar"
    if ease >= 50:
        return "algo dificil"
    if ease >= 30:
        return "dificil"
    return "muy dificil (nivel universitario)"


def main():
    if len(sys.argv) != 2:
        print("usage: readability_scorer.py <url|page.json>", file=sys.stderr)
        sys.exit(2)

    page = load_page(sys.argv[1])
    text = page.get("text", "")
    sentence_count, word_count, syllable_count = analyze(text)
    result = compute_scores(sentence_count, word_count, syllable_count)

    print(f"Oraciones: {sentence_count}  Palabras: {word_count}  Silabas: {syllable_count}")
    if result is None:
        print("No hay suficiente texto para calcular un puntaje.")
        return

    ease, grade, words_per_sentence, syllables_per_word = result
    print(f"Palabras por oracion: {words_per_sentence:.1f}  Silabas por palabra: {syllables_per_word:.2f}")
    print(f"Flesch Reading Ease: {ease:.1f} ({interpret(ease)})")
    print(f"Flesch-Kincaid Grade Level: {grade:.1f}")


if __name__ == "__main__":
    main()
