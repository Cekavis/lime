"""Organize ignored developer source snapshots into UTF-8 corpus folders.

This maintenance helper uses only Python's standard library. Segmentation and
pinyin validation belong to the Rust runtime. Use --check to compare existing
fixtures with their source snapshots without writing anything.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
DATA = ROOT / "crates" / "lime-benchmark" / "data"
SOURCES = DATA / "sources"
OUTPUT = DATA / "corpora"
CATEGORIES = {"zhihu": "知乎", "classics": "经典文章"}
ZH_HEADER = re.compile(r"^链接：https://www\.zhihu\.com/question/\d+\r?\n", re.MULTILINE)


def prepare() -> dict[Path, bytes]:
    manifest = json.loads((SOURCES / "manifest.json").read_bytes())
    articles: dict[Path, bytes] = {}
    question_number = 0
    for source in manifest["sources"]:
        source_path = (SOURCES / source["file"]).resolve()
        if not source_path.is_relative_to(SOURCES.resolve()):
            raise ValueError(f"source path leaves the snapshots directory: {source['file']}")
        raw = source_path.read_bytes()
        if hashlib.sha256(raw).hexdigest() != source["sha256"]:
            raise ValueError(f"source checksum mismatch: {source['file']}")
        # Decode bytes directly so original newlines survive on every platform.
        text = raw.decode("utf-8")
        category = CATEGORIES[source["category"]]
        if source["category"] == "classics":
            filename = source["file"]
            expected_filename = f"{source['name'].strip()} {source['author'].strip()}.txt"
            if filename != expected_filename or Path(filename).name != filename or Path(filename).suffix.lower() != ".txt":
                raise ValueError(f"invalid classic article filename: {filename}")
            path = OUTPUT / category / filename
            if path in articles:
                raise ValueError(f"duplicate article: {path}")
            articles[path] = raw
            continue
        headers = list(ZH_HEADER.finditer(text))
        if not headers or headers[0].start() != 0:
            raise ValueError(f"Zhihu snapshot must begin with a question URL header: {source_path}")
        for index, header in enumerate(headers):
            question_number += 1
            end = headers[index + 1].start() if index + 1 < len(headers) else len(text)
            articles[OUTPUT / category / f"{question_number:03}.txt"] = text[header.end():end].encode("utf-8")
    return articles


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify ignored fixtures without writing")
    args = parser.parse_args()
    expected = prepare()
    actual = {path: path.read_bytes() for path in OUTPUT.glob("*/*.txt")}
    extra = sorted(str(path) for path in actual.keys() - expected.keys())
    if args.check:
        missing = sorted(str(path) for path in expected.keys() - actual.keys())
        changed = sorted(str(path) for path in expected.keys() & actual.keys() if expected[path] != actual[path])
        if missing or extra or changed:
            raise SystemExit(f"Fixture files differ; missing={missing}, extra={extra}, changed={changed}")
    else:
        if extra:
            raise SystemExit(f"Unrecognized articles must be reviewed before regeneration: {extra}")
        for path, content in expected.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
    for category in CATEGORIES.values():
        print(f"{category}: {sum(path.parent.name == category for path in expected)} articles")


if __name__ == "__main__":
    main()
