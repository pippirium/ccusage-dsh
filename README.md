# ccusage-dsh

[ccusage](https://github.com/ccusage/ccusage) with a local DeepSeek Harness (DSH) adapter. Running **`ccusage` with no arguments includes DSH automatically**, alongside the upstream sources. DSH costs are always **$0**; recorded token counts are preserved.

This is an independent MIT-licensed distribution, not an official ccusage release or a GitHub fork. Upstream is pinned as a Git submodule; the adapter and a small integration patch are maintained here. No upstream contribution is planned.

## Install from source

Requires Git, tar, Python **3.10+**, [rustup](https://rustup.rs/), a C/C++ build toolchain (Xcode Command Line Tools on macOS; `build-essential` on Debian/Ubuntu), and network access for the first build. Rustup installs the toolchain specified by upstream (currently Rust 1.97.1). Node.js, npm, Nix, and a `zstd` executable are not required at runtime.

```sh
git clone --recurse-submodules https://github.com/pippirium/ccusage-dsh.git
cd ccusage-dsh
python3 scripts/build.py --test
python3 scripts/build.py
python3 scripts/smoke.py
python3 scripts/install.py
```

The installer places a symlink at `~/.local/bin/ccusage`; ensure that directory is on `PATH` before any other ccusage installation. It preserves the previous executable or npm symlink for rollback. It does not uninstall the upstream npm package. Keep this checkout and its `dist/ccusage` after installation. The build and symlink installer currently target macOS/Linux.

```sh
ccusage --version                # 20.0.44+dsh.1 for the current pin
ccusage                          # all detected sources, including DSH
ccusage --json --by-agent         # machine-readable per-agent breakdowns
ccusage dsh daily                 # DSH only
ccusage dsh weekly
ccusage dsh monthly
ccusage dsh session
ccusage dsh daily --since 20261001 --timezone Asia/Tokyo
python3 scripts/install.py --uninstall  # restore the previous ccusage
```

DSH's home defaults to `~/.dsh`. Set `DSH_HOME` to use another home; the adapter reads `$DSH_HOME/sessions`. Session data stays on your machine. `--offline` uses upstream's embedded pricing for other agents; DSH itself does not fetch pricing. The source build fetches the pricing snapshot pinned by upstream's `flake.lock`.

## Accounting

- Reads Zstandard-compressed `session.v3.jsonl.zstd` and `session.v4.jsonl.zstd` recursively, including child sessions. When both versions exist in one directory, v4 takes precedence.
- Uses `assistant/message.data.usage` as the committed-message authority. Its embedded stream is only a fallback; it is never added again.
- Repeated settlements for the same turn/step replace the previous accounting snapshot, matching DSH token-meter. `llm/retry-started` closes that slot so retries count separately.
- Counts `assistant/attempt` usage for failed, cancelled, and retried requests when recorded. Uses the final usage snapshot in an attempt's stream.
- DSH input counts are disjoint: `inputTokens` is uncached input, while cache reads and writes are separate. Total usage includes all three input categories plus output; any positive remainder in `totalTokens` is preserved. Reasoning is not added a second time on top of output.
- Excludes inherited history before `session/end-seed` in sessions whose header has `isSeeded: true`. Ordinary source sessions retain all their usage.
- Deduplicates copied logs by session ID and event sequence. Conflicting copies, unsupported versions, malformed complete records, and decompression errors fail visibly. Incomplete active-file tails emit a warning and count only complete records.
- No token estimates are fabricated when the provider did not report usage. Consequently, missing provider accounting cannot be recovered from these logs.
- DSH cost is unconditionally $0, including `--mode calculate`. This is a reporting policy, not an estimate of electricity or paid API charges. Other sources keep upstream's cost behavior.

The adapter reads only the usage and routing metadata needed for reports; it does not export conversation text. All committed test data is synthetic.

## Layout and maintenance

- `upstream/`: unmodified ccusage submodule at a fixed commit.
- `adapter/`: native Rust DSH adapter and fixture-backed tests.
- `patches/dsh.patch`: registers DSH in the native CLI and unified reports.
- `Cargo.lock`: dependency lock for the patched workspace.
- `scripts/build.py`: exports the pin into ignored `.build/source`, creates an isolated Git root, applies the patch, copies the adapter, and builds. The submodule stays clean.
- `scripts/smoke.py`: verifies the actual executable with synthetic compressed logs, including plain `ccusage` and existing-source coexistence.
- `dist/`: ignored local executable; private session data is never copied into the repository.

To update upstream, intentionally advance the submodule pin, adapt the patch, regenerate the patched workspace lock file, and run tests and the executable smoke check before rebuilding. The patch must apply cleanly; do not auto-track upstream `main`. The build currently targets macOS/Linux; Windows source builds have not been validated.

## License

MIT, matching upstream. The original copyright and license are preserved in [LICENSE](LICENSE) and in the submodule. The DSH adapter and integration code are also MIT licensed. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for attribution.
