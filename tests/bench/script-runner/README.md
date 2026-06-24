# Script-runner dispatch benchmark

Measures how fast each tool looks up a `package.json` script and dispatches it. The canonical comparison is `nub run` vs `node --run`; pass `--tools all` to include npm and pnpm.

## Quick run

```bash
cd /path/to/dun
cargo build --release -p nub-cli
bash tests/bench/script-runner/run-vs-node.sh --fixture true
```

Benchmark a different binary:

```bash
NUB="$PWD/target/release/nub" bash tests/bench/script-runner/run-vs-node.sh --fixture true
```

## Fixtures

The harness creates temporary no-dependency fixture projects.

```json
{
  "name": "vs-node-bench",
  "version": "1.0.0",
  "scripts": {
    "noop": "true"
  }
}
```

The `true` fixture isolates runner dispatch. The script body is a shell builtin that exits successfully and does essentially no work.

```json
{
  "name": "vs-node-bench",
  "version": "1.0.0",
  "scripts": {
    "noop": "node -e \"\""
  }
}
```

The `node -e ""` fixture measures dispatch plus an empty Node process.

## Commands

```bash
bash tests/bench/script-runner/run-vs-node.sh --fixture true
bash tests/bench/script-runner/run-vs-node.sh --fixture node-e
bash tests/bench/script-runner/run-vs-node.sh --fixture both
bash tests/bench/script-runner/run-vs-node.sh --fixture true --tools all
```

For a quieter publication-grade run:

```bash
bash tests/bench/script-runner/run-vs-node.sh --fixture true --runs 100 --prewarm 30 --warmup 30 --max-load 2 --save
```

For a clean screenshot (preamble suppressed, hyperfine output only):

```bash
bash tests/bench/script-runner/run-vs-node.sh --quiet
# or: --clean (alias)
```

`--quiet`/`--clean` suppresses the banner, version info, and exit-0 verification output. The verification still runs — a failure still errors loudly. All three harnesses (`run-vs-node.sh`, `run-pure.sh`, `run-legacy.sh`) accept the flag.

## Results

By default, scripts write JSON to a temp directory. Pass `--save` to update checked-in JSON under `tests/bench/script-runner/results/`.

The older `run-pure.sh` and `run-legacy.sh` harnesses are kept for historical comparisons. Use `run-vs-node.sh` for the published `node --run` comparison.

## Requirements

- `hyperfine`
- Node 22 or newer
- `npm`
- `pnpm`
- Rust / Cargo
