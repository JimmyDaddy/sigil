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
  --expected-version 0.0.1-alpha.4 \
  --expected-commit f4e6c5aeea86b3283988efe20db44a0f97454f97
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

## Evidence

The default output is `.repo-local-dev/dogfood/offline-<timestamp>`. The aggregate `manifest.json`, `manifest.sha256`, and `summary.md` contain only timestamps, build identity, binary digest, case status, duration, and relative evidence paths. Raw case artifacts stay in the ignored local output directory for debugging and are never uploaded automatically. A custom output outside the repository is recorded as explicitly selected local output; a custom output inside the repository is rejected unless Git ignores it.

When an outer deadline fires, the runner first interrupts the case harness so its cleanup handlers can restore terminal or clipboard state, then terminates any remaining detached descendants.

Loopback success proves local application wiring and product interaction. It does not prove remote-provider quality or billing behavior. Provider-backed campaigns remain explicit and use the cost/deadline controls documented in [Model-backed evaluation](model-evaluation.md).
