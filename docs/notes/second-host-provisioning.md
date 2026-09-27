# Second-host (lab) provisioning — cgov + claude-print runbook

codinghome is the primary governor host, but it is not the only one. The lab
host (Tailscale IP `100.81.129.38` — the hostname times out, use the IP) runs
the same stack, and before this file the whole of the in-repo documentation for
that was CLAUDE.md §3's one-line lab note: *each host needs its own
`claude-print` binary + adapters + creds*. This is the runbook for standing a
second host up and for keeping it in parity with the primary afterwards.

Like [deployment-verification.md](deployment-verification.md), this contains
**no recorded evidence**: it is commands and criteria, not state. Point-in-time
evidence belongs on beads.

## The per-host parity surface

Everything below must exist **on each host independently**. Nothing here is
copied from the primary — every artifact is built, installed, or authenticated
on the host that will use it.

| Surface | Path | Established by |
|---|---|---|
| cgov binary | `~/.local/bin/cgov` | `install.sh` (digest-verified release) or a build on the host |
| claude-print real binary | typically `~/.cargo/bin/claude-print` | built on the host from the claude-print repo |
| claude-print dispatch path | `/home/coding/.local/bin/claude-print` | `deploy/install-claude-print-adapters.sh` (symlinks the real binary — the adapter templates call this absolute path because NEEDLE's dispatch shell PATH is not the interactive shell's) |
| NEEDLE adapters | `~/.config/needle/adapters/claude-print-*.yaml` | the same installer, byte-copies of `deploy/needle-adapters/` — **not** `~/.needle/agents/`, a stale staging path the current `needle` binary does not read |
| governor config | `~/.config/claude-governor/governor.yaml` | `cgov init` seeds it; live values are edited on this machine path — the repo's `config/governor.yaml` is a build-time seed template, never running configuration |
| subscription credentials | `~/.claude/.credentials.json` | `claude login` run **on that host** |

Path-layout constraint: the committed adapter templates call
`/home/coding/.local/bin/claude-print` by absolute path, so a second host must
have the same user/home layout (`coding` at `/home/coding`). The installer
establishes exactly the path it finds in the templates, so the constraint is
self-enforcing — a host with a different layout fails at the symlink step, not
silently at dispatch.

## Step 1 — reach the host

```bash
ssh coding@100.81.129.38
```

The hostname times out; the Tailscale IP is the stable address.

## Step 2 — cgov binary

Either the digest-verified release (pin the version — see the parity section
below for why both hosts should run the same one):

```bash
curl --netrc -fsSL https://git.ardenone.com/jedarden/claude-governor/raw/branch/main/install.sh \
  | CGOV_VERSION=vX.Y.Z bash
```

…or build on the host from a clone of this repo (the wrapper-free path; the
lab host's `cargo` is the NEEDLE wrapper, which falls back to a
cgroup-limited local build for non-test commands — that is fine here):

```bash
cd ~/claude-governor && make install    # builds release, copies to ~/.local/bin
```

## Step 3 — claude-print binary on the host

The dispatch path is a symlink to a real binary that must already exist on
this host. Build it here (do not copy a binary or a credentials file over from
the primary):

```bash
cd ~/claude-print && cargo build --release && cargo install --path .
```

## Step 4 — adapters (installer + static gates)

The installer is source-tree only (`install.sh` does not ship `deploy/`), so
this step needs a clone of this repo on the host — reuse the shared checkout;
do not create a disposable one. Run it from the checkout whose commit matches
the installed cgov build (its runtime sync check reads
`src/adapter_verify.rs` from that checkout):

```bash
cd ~/claude-governor
./deploy/install-claude-print-adapters.sh            # full: static gates + live probe
./deploy/install-claude-print-adapters.sh --skip-live # offline / quota-conserving
```

The installer installs the adapter YAMLs, links `/home/coding/.local/bin/claude-print`
to the real binary, and runs the authoritative verifications: the rule-3/rule-5
env-scrub static check, the full invoke-contract check (prompt redirection,
`--pretrust-cwd`, `--output-format stream-json`, `--no-inherit-hooks`, exact
`timeout_secs` pins), the installer↔Rust constant sync check, and — without
`--skip-live` — one trivial live dispatch per adapter. `needle test-agent` is
**not** sufficient proof (it can report READY while every dispatch dies at
exit 127); the installer and `cgov doctor` are the authoritative checks.

## Step 5 — credentials, on the host

```bash
claude login
```

Authenticate interactively **on the second host**. Do not copy
`~/.claude/.credentials.json` from the primary: that would duplicate a live
bearer credential across hosts, and after that neither host's doctor checks
can attribute a leak or an anomaly to a host. Each host refreshing its own
grant is the supported posture. governor.yaml carries no secrets — anything
secret-shaped belongs in OpenBao under the instance that owns the prefix, with
the config carrying only the retrieval path.

## Step 6 — governor config and services

```bash
cgov init          # seeds ~/.config/claude-governor/governor.yaml, installs units
cgov config --edit # add the agents this host should run — the pools need not mirror the primary's
cgov enable        # enable + start observe, act, token collector
```

Config is per-host on purpose: the two hosts may run different pools, and each
reads only its own machine-path config at start. A daemon reload picks up
edits only on restart (`cgov restart`).

## Step 7 — verify

Apply [deployment-verification.md](deployment-verification.md) — including its
step-0 warm-up rule: doctor output is not interpretable for ~30 minutes after
a first start (`burn_rate_samples`, `prediction_accuracy`, `log_file` are
cold-start-degraded by design). Two checks in the report are the
claude-print-specific pass condition for this runbook:

- `claude_print_adapters` — the installed templates satisfy the static
  contract (and, without `--skip-live`, answer a live trivial dispatch).
- `claude_print_parity` — the installed templates are byte-identical to the
  canonical templates this cgov build carries, and the dispatch path exists
  and is executable. See below.

## Keeping parity after provisioning

`cgov doctor`'s `claude_print_parity` check is the drift detector. It compares
the host's installed `claude-print-*.yaml` files byte-for-byte against the
templates embedded in the running cgov binary at build time, and verifies
every absolute dispatch path the canonical templates call exists here. Run
`cgov doctor` on the second host after any of the triggers below; a FAIL names
the drifted file or the missing path, and its remediation is the installer —
never hand-editing the live template back into shape.

| Trigger | Action on BOTH hosts |
|---|---|
| `deploy/needle-adapters/*` or the `src/adapter_verify.rs` pins change in this repo | rebuild/reinstall cgov (the binary embeds the canonical templates it checks against), then re-run `deploy/install-claude-print-adapters.sh` |
| a new cgov release is installed on one host | install the **same** `CGOV_VERSION` on the other |
| the claude-print repo changes | rebuild claude-print on the host (the symlink's target), re-run the installer to re-verify |
| anything under `~/.config/needle/adapters/` was touched by hand | expect `claude_print_parity` FAIL on that host; resolve by re-running the installer |

Version alignment is what makes parity meaningful: the check is against the
binary's own embedded templates, so a host frozen in time (old cgov plus
matching old adapters) passes parity while the repo has moved on. Keep
`cgov version` identical across hosts, and bump both together — the same
apply-on-both discipline NEEDLE's `apply-lab-fleet.sh` + `wrapper-drift.timer`
give the cargo wrappers. There is deliberately no timer for this one: the
parity check runs where doctor runs, and an operator or agent verifying the
host (deployment-verification.md) is the trigger. If dispatch-volume ever
justifies a timer, it belongs in that contract test's surface, not hand-rolled
here.

## What parity does not cover

- Different pools per host are **not** drift: governor.yaml is per-host by
  design; only the claude-print surface (adapters + dispatch path) is pinned
  to the canonical templates.
- A stale-but-self-consistent host passes parity (see the version-alignment
  note above) — repo-side changes reaching both hosts is a process duty, not
  something the host can detect alone.
- The non-claude-print adapters in `~/.config/needle/adapters/` (glm, codex,
  opencode, …) are outside this contract; the parity check compares only the
  `claude-print-*.yaml` namespace this repo ships.
