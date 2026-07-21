# Pairwise annotation

The annotation page is a dependency-free local UI for the Phase 4 comparison
contract. It never displays model identity, generation time, or tool counts.

1. Run `atelier-lab bundle <pairs.jsonl> <output-dir>` to produce
   `comparisons.jsonl` and one portable artifact directory. The lower-level
   `write_comparisons_jsonl` API remains available to custom pipelines.
2. Choose the bundle's `artifacts/` directory in the page. Files retain the
   `sha256/<prefix>/<hash>` layout; the page indexes them by hash.
3. Open `index.html`, choose the comparison file and artifact directory, enter
   an opaque annotator id, and label the pairs.
4. Download `annotations.jsonl`. Keep annotator ids pseudonymous; do not put
   names or email addresses in the dataset.
5. Run `atelier-lab export-critic <comparisons.jsonl> <annotations.jsonl>
   <critic.jsonl>` to validate and remove randomized presentation order before
   training.

The page randomizes left/right placement for every comparison and records the
presented candidate ids, so labels can always be converted back to canonical
candidate A/B order. It works directly from disk in Chromium-based browsers;
no local server or network connection is required.
