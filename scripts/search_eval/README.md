# ctx search eval harnesses

These scripts are local-only dogfood tools for source-backed lexical,
semantic, and hybrid search.
They may read private agent-history queries, snippets, local paths, and ctx IDs.
Do not publish raw outputs without review.

## Scripts

- `default_experience_gate.py` runs `ctx search` across hybrid, lexical, and
  semantic modes. Its output is private because it includes query
  text and top snippets.
- `private_eval.py` scores a private JSONL manifest with hashed expected IDs.
  Its output is private unless the manifest and output have been separately
  reviewed.
- `semantic_worker_bench.py` records worker/search/status timing and
  `search/semantic` generation size for local performance work.
- `soak_runner.py` reads those private reports and writes a private-safe
  aggregate promotion report. It exits nonzero when configured thresholds fail.

## Threshold File

`soak_runner.py --thresholds thresholds.json` accepts a JSON object. Missing
thresholds use conservative defaults.

```json
{
  "basics": {
    "require_ok": true,
    "require_refresh_background": true,
    "hybrid_p95_vs_lexical_max_ratio": 2.0,
    "max_hybrid_p95_ms": 2000,
    "require_hybrid_fallback_lexical": true
  },
  "private_eval": {
    "baseline": "lexical",
    "candidate": "hybrid",
    "min_hit5_delta": 0.0,
    "min_mrr_delta": 0.0,
    "max_p95_ratio": 2.0
  },
  "status": {
    "require_semantic_dirty_zero": false,
    "min_semantic_coverage_ratio": null
  }
}
```

## Example

```bash
python3 scripts/search_eval/default_experience_gate.py \
  --ctx-bin "$PWD/target/release/ctx" \
  --data-root "$HOME/.ctx-semantic-soak" \
  --refresh background \
  --query-set ./private-default-gate.json \
  --output "$HOME/.ctx-semantic-soak/default-gate.json"

python3 scripts/search_eval/soak_runner.py \
  --basics-report "$HOME/.ctx-semantic-soak/default-gate.json" \
  --status-json "$HOME/.ctx-semantic-soak/status.json" \
  --thresholds ./private-thresholds.json \
  --output "$HOME/.ctx-semantic-soak/soak-summary.json"
```

The summary intentionally omits raw queries, snippets, UUIDs, local paths, and
provider IDs. Keep the raw input reports private.
