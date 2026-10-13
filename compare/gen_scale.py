"""Filler pages for the speed run at size: N pages made from the other five scenarios' pages (never
shopline's, so its questions keep their answers), each with a title, aliases and an id of its own.

    python gen_scale.py 10000      -> scale/pages.jsonl  ({"id", "text"} per line)
"""

import json
import pathlib
import random
import sys

import corpus

HERE = pathlib.Path(__file__).parent


def main():
    n = int(sys.argv[1])
    rnd = random.Random(7)
    sources = [p for t in corpus.tenants() if t != "shopline" for p in corpus.pages(t)
               if p["scope"] == "t" and not p["id"].startswith("graph/")]
    out = HERE / "scale" / "pages.jsonl"
    out.parent.mkdir(exist_ok=True)
    with out.open("w", encoding="utf-8") as f:
        for i in range(n):
            src = rnd.choice(sources)
            head, _, body = src["text"].replace("\r\n", "\n").partition("\n---\n")
            fields = dict(l.split(":", 1) for l in head.strip("-\n").splitlines() if ":" in l)
            title = fields.get("title", "").strip()
            code = f"F{i:05d}"
            text = (f"---\nkind: expertise\ntitle: {title} ({code})\n"
                    f"aliases: [{code}, 资料{code}, file {code}]\n"
                    f"keywords: [{fields.get('keywords', '[]').strip(' []')}]\n"
                    f"summary: {fields.get('summary', '').strip()}\n"
                    f"about: []\nsources: [filler-{code}.json]\nupdated: 2026-09-30\n---\n{body}")
            f.write(json.dumps({"id": f"expertise/filler-{code.lower()}", "text": text}, ensure_ascii=False) + "\n")
    print(n, "pages from", len(sources), "sources")


if __name__ == "__main__":
    main()
