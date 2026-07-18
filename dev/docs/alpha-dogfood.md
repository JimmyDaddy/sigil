# Alpha Dogfood Campaigns

Alpha dogfood is release evidence, not telemetry. Sigil does not upload campaign artifacts, session history, prompts, tool material, credentials, or workspace content.

## Public distribution smoke

After publishing an alpha, manually run `Published Distribution Smoke`. The workflow installs the current npm alpha on Linux, Windows, macOS ARM and macOS Intel; installs the public Homebrew formula on both macOS architectures; and verifies GitHub Release checksums and artifact attestations.

The workflow is read-only. It cannot publish packages, create releases, move npm dist-tags, or update the tap.

## Offline production-binary campaign

Select a standalone native Sigil executable explicitly, such as the binary from a Release archive or Homebrew installation:

```bash
python3 scripts/alpha-dogfood-campaign.py \
  --binary /path/to/sigil \
  --expected-version 0.0.1-alpha.5 \
  --expected-commit 8d57b6dda05561b791f908d6a6a9a3f693cae121
```

Use `--expected-sha256` when a binary digest is available. Before creating any case state, the runner copies the executable into a private temporary directory, hashes that frozen copy, and inspects its `sigil --version`. Every case executes the same admitted copy, so replacing the source binary during a campaign cannot drift the evidence. The runner never builds a binary implicitly.

Script launchers, including the npm JavaScript launcher, are intentionally not accepted because freezing a wrapper would not freeze the binary it delegates to. Admission requires a Mach-O, ELF, or PE executable. Public npm installation and launcher execution are covered by Public Distribution Smoke; use the native binary inside the installed platform package only when a local offline campaign is also required.

The default campaign runs Context V1, Web V1, feedback, terminal attention, and image-input acceptance through real headless or PTY product entrypoints. Each case receives isolated HOME, XDG, state, cache and temporary roots. Provider credentials and Sigil config overrides are not inherited, ambient proxy routes point to a closed loopback endpoint, and every selected harness configures and inspects case-owned loopback services. The Feedback case records its loopback request count and rejects provider-generation requests.

This is not an OS-level socket sandbox. The offline claim is limited to these reviewed case definitions, their loopback endpoint assertions, and the absence of ambient credentials/configuration.

On a non-macOS or headless host, image clipboard coverage must be skipped explicitly:

```bash
python3 scripts/alpha-dogfood-campaign.py \
  --binary /path/to/sigil \
  --skip-clipboard
```

Use repeated `--case` arguments to select a subset. `--list-cases` prints the stable case ids.

## Stateful TUI campaign

Run the stateful campaign separately after the default offline campaign. It requires the exact native binary plus the checksum-pinned DeepSeek V4 Flash `tokenizer.json`. Install the tokenizer first if needed; the command prints its verified path:

```bash
sigil tokenizer install deepseek-v4-flash

python3 scripts/tui-stateful-pty-acceptance.py \
  --binary /path/to/sigil \
  --tokenizer-json /path/printed/by/tokenizer-install/tokenizer.json \
  --expected-version 0.0.1-alpha.5 \
  --expected-commit 8d57b6dda05561b791f908d6a6a9a3f693cae121 \
  --expected-binary-sha256 <sha256>
```

The campaign freezes both inputs into case-owned storage and runs three real TUI processes over one durable session family:

1. a loopback provider creates four finalized turns, including one controlled `write_file` mutation and the facts-before-final continuation that previously risked a duplicate reply;
2. after the loopback server is closed, a default-transport configuration with closed ambient proxies resumes the source session, applies locally admitted compaction, restores the controlled file through the Ctrl-R reverse-diff modal, and uses modal `F` to fork without changing the restored file;
3. a fresh process starts on the source session and uses the visible `/resume` selector to switch to the unique non-current fork.

Passing evidence requires the final reply to appear exactly once on both reconstructed VT screens and exactly once as a structured final-answer entry in each source/fork stream. It also requires one `compaction_applied_v2`, one `checkpoint_restored`, one `conversation_forked`, and unchanged file hashes across fork and resume. Custom provider routes are intentionally not used for local compaction admission.

The safe `manifest.json` contains only public binary/tokenizer identity, counters, booleans, duration, and relative evidence paths/checksums. Byte-exact source/fork JSONL plus raw PTY logs remain in ignored local output for independent recounting and are never uploaded. Repository-local output is rejected unless Git ignores it; an explicitly selected path outside the repository is recorded as local-only. CI runs parser, admission, process-cleanup, durable-structure, and manifest-privacy contract tests; the real release-binary campaign remains an explicit local release check because it requires the installed tokenizer artifact.

## Real-provider campaign

After the offline and stateful tiers pass, run `scripts/real-provider-dogfood-campaign.py` with an explicit native binary, config, case list, repetition count, cost budget, and deadline. The stable R34.4 matrix contains `small-code-edit`, `stale-after-write`, `workspace-trust`, `sandbox-denial`, and `plan-only`; the exact command and evidence contract are documented in [Model-backed evaluation](model-evaluation.md#run-the-rfc-0034-dogfood-matrix).

This tier is allowed to contact the configured provider and spend tokens. Its aggregate manifest contains case terminals and public binary identity only; raw model, PTY, and session evidence remains local. The budget is a pre-dispatch admission/accounting boundary, not a provider-side cap for an already dispatched request.

## Evidence

The default output is `.repo-local-dev/dogfood/offline-<timestamp>`. The aggregate `manifest.json`, `manifest.sha256`, and `summary.md` contain only timestamps, build identity, binary digest, case status, duration, and relative evidence paths. Raw case artifacts stay in the ignored local output directory for debugging and are never uploaded automatically. A custom output outside the repository is recorded as explicitly selected local output; a custom output inside the repository is rejected unless Git ignores it.

When an outer deadline fires, the runner first interrupts the case harness so its cleanup handlers can restore terminal or clipboard state, then terminates any remaining detached descendants.

Loopback success proves local application wiring and product interaction. It does not prove remote-provider quality or billing behavior. Provider-backed campaigns remain explicit and use the cost/deadline controls documented in [Model-backed evaluation](model-evaluation.md).
