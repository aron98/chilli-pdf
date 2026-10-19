# Preservation fixtures

Committed, hand-written minimal PDFs used by `tests/preservation.rs`
to prove that the overlay merge preserves metadata, bookmarks, links,
and page structure. They are intentionally tiny (< 2 KB each) so the
tests can reason about every object in them.

| File | Carries |
|------|---------|
| `metadata.pdf` | 2 pages; indirect `/Info` with UTF-16BE title, dates, custom key; shared indirect `/Resources` with an existing Type1 font |
| `outline.pdf` | 3-level outline (4 items, `/Dest` arrays and a GoTo action) |
| `links.pdf` | 2 internal link annotations (`/Dest` and `/A /GoTo`) + 1 external `/URI` link |
| `rotated_metadata.pdf` | 1 page, `/Rotate 90`, inline empty resources, Info dict with Unicode title |

All files are classic-xref PDF 1.4, pass `qpdf --check`, and are
regenerated deterministically (no network, stdlib only) with:

```sh
python3 tests/fixtures/preservation/make_fixtures.py
```

The generator writes the fixtures next to itself; run it from the
repository root and commit the result if the fixtures ever need to
change.
