# Pairwise annotation

The annotation page is a dependency-free local UI for the Phase 4 comparison
contract. It never displays model identity, generation time, or tool counts.

1. Produce a `PairwiseComparison` JSON array or JSONL file with
   `write_comparisons_jsonl`.
2. Collect the referenced artifact stores into one directory. Files may retain
   the `sha256/<prefix>/<hash>` layout; the page indexes them by hash.
3. Open `index.html`, choose the comparison file and artifact directory, enter
   an opaque annotator id, and label the pairs.
4. Download `annotations.jsonl`. Keep annotator ids pseudonymous; do not put
   names or email addresses in the dataset.

The page randomizes left/right placement for every comparison and records the
presented candidate ids, so labels can always be converted back to canonical
candidate A/B order. It works directly from disk in Chromium-based browsers;
no local server or network connection is required.
