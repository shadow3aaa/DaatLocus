# Train Sources

- `swe_bench_train_sample.json`: official SWE-bench train split sample (10 tasks), converted to the local `SweTrainSource` JSON format.
- Source dataset: `SWE-bench/SWE-bench` train split via Hugging Face datasets-server rows API.
- Intended for fast local/VM smoke tests with `train-source inspect|learn`.
