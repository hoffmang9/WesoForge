# bbr_client

CLI compactor worker:

- Leases one job from `bbr_backend`
- Computes a compact VDF proof witness via the fast `chiavdf` engine
- Submits the result back to the backend
- Loops forever

## Run

```bash
cd bbr_client
cargo run -p bbr-client -- \
  --backend-url http://127.0.0.1:8080
```

The client will sleep 10 seconds when no jobs are available.

## Output

- Default: in-place progress line when stdout is a TTY.
- `--no-tui` (or `BBR_NO_TUI=1`): newline progress logs (friendlier for piping / `tee`).

## Benchmark

Run a fixed local proof benchmark and exit:

```bash
cd bbr_client
cargo run -p bbr-client -- --bench 0
```
