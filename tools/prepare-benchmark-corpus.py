"""Rebuild the offline benchmark from checked-in source snapshots.

Install generation-only dependencies with:
    py -3 -m pip install jieba==0.42.1 pypinyin==0.55.0
Then run this script with --check to verify reproducibility without writing files.
No network access or model/Rime runtime is required.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.metadata
import itertools
import json
import re
from pathlib import Path

import jieba
from pypinyin import Style, lazy_pinyin


ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "crates" / "lime-benchmark" / "data"
SOURCES = DATA / "sources"
DEPENDENCIES = {"jieba": "0.42.1", "pypinyin": "0.55.0"}
HAN = re.compile(r"[\u3007\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\U00020000-\U000323af]+\Z")
ZH_HEADER = re.compile(r"^链接：https://www\.zhihu\.com/question/\d+\r?\n", re.MULTILINE)


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def source_documents(source: dict, text: str):
    if source["category"] == "classics":
        yield source["id"], 0, len(text), text
        return
    headers = list(ZH_HEADER.finditer(text))
    if not headers or headers[0].start() != 0:
        raise ValueError("Zhihu source must begin with a question URL header")
    for index, header in enumerate(headers):
        start = header.end()
        end = headers[index + 1].start() if index + 1 < len(headers) else len(text)
        # Keep every body character, including line breaks between answers.
        yield f"{source['id']}-{index + 1:03}", start, end, text[start:end]


def prepare() -> dict:
    for name, version in DEPENDENCIES.items():
        actual = importlib.metadata.version(name)
        if actual != version:
            raise ValueError(f"{name} must be {version}, found {actual}")
    manifest_bytes = (SOURCES / "manifest.json").read_bytes()
    manifest = json.loads(manifest_bytes)
    documents = []
    counts = {"zhihu": {"characters": 0, "cases": 0}, "classics": {"characters": 0, "cases": 0}}
    tokenizer = jieba.Tokenizer()
    tokenizer.initialize()
    for source in manifest["sources"]:
        raw = (SOURCES / source["file"]).read_bytes()
        if digest(raw) != source["sha256"]:
            raise ValueError(f"source checksum mismatch: {source['file']}")
        # Decode bytes directly: do not let platform newline conversion alter offsets.
        text = raw.decode("utf-8")
        for ident, source_start, source_end, body in source_documents(source, text):
            words = []
            previous_end = 0
            chinese_words = 0
            for token, token_start, token_end in tokenizer.tokenize(body, mode="default", HMM=True):
                if token_start != previous_end or body[token_start:token_end] != token:
                    raise ValueError(f"tokenization does not cover the original text: {ident}")
                # Mixed dictionary entries such as A股 / B超 must not silently
                # discard their Chinese part when letters are excluded.
                for is_chinese, group in itertools.groupby(
                    enumerate(token, token_start), key=lambda item: bool(HAN.fullmatch(item[1]))
                ):
                    run = list(group)
                    start, end = run[0][0], run[-1][0] + 1
                    word = body[start:end]
                    syllables = []
                    if is_chinese:
                        syllables = lazy_pinyin(word, style=Style.NORMAL, errors="default", v_to_u=False)
                        if len(syllables) != len(word) or not all(re.fullmatch(r"[a-z]+", s) for s in syllables):
                            raise ValueError(f"invalid pinyin for {ident} token at {start}: {word!r}")
                        chinese_words += 1
                    words.append([start, end, syllables])
                previous_end = token_end
            if previous_end != len(body) or chinese_words < 2:
                raise ValueError(f"incomplete/empty document: {ident}")
            category_counts = counts[source["category"]]
            category_counts["characters"] += sum(bool(HAN.fullmatch(ch)) for ch in body)
            category_counts["cases"] += chinese_words - 1
            documents.append({
                "id": ident,
                "category": source["category"],
                "source_id": source["id"],
                "source_start": source_start,
                "source_end": source_end,
                "text": body,
                "words": words,
            })
    if counts["classics"]["characters"] < counts["zhihu"]["characters"]:
        raise ValueError("classics must contain at least as many Chinese characters as Zhihu")
    dictionary = Path(jieba.__file__).parent / "dict.txt"
    return {
        "id": "lime-real-text-v1",
        "name": "知乎与经典文章逐词评测",
        "version": 1,
        "generator": {
            **DEPENDENCIES,
            "jieba_mode": "default",
            "jieba_hmm": True,
            "jieba_dictionary_sha256": digest(dictionary.read_bytes()),
            "source_manifest_sha256": digest(manifest_bytes),
            "offset_unit": "unicode_scalar",
        },
        "corpora": [
            {"id": category, "name": name, **counts[category]}
            for category, name in [("zhihu", "知乎"), ("classics", "经典文章")]
        ],
        "documents": documents,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify the frozen corpus without writing")
    args = parser.parse_args()
    result = prepare()
    encoded = (json.dumps(result, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
    output = DATA / "corpus.json"
    if args.check:
        if output.read_bytes() != encoded:
            raise SystemExit("Frozen corpus differs; regenerate and review the source/token changes.")
    else:
        output.write_bytes(encoded)
    for corpus in result["corpora"]:
        print(f"{corpus['id']}: {corpus['characters']} Chinese characters, {corpus['cases']} cases")
    print(f"corpus SHA-256: {digest(encoded)}")


if __name__ == "__main__":
    main()
