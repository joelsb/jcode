#!/usr/bin/env python3
"""Health check for the local jcode memory pipeline.

Answers the question "is memory actually working right now?" from evidence on
disk, not from config. Built after a 9-day silent outage (2026-08-04..15) in
which a stale sidecar model id produced 67 judge 404s, and the degraded
maintenance path decayed the global memory graph from 2 entries to 0 without
ever surfacing an error. See docs/adr/0021-memory-decay-requires-a-verdict.md.

Checks, each of which failed to fire during that outage:

  1. EXTRACTION      periodic extractions are not erroring out
  2. WRITES          memories are actually being written, not just attempted
  3. DECAY           no entry sits below the 0.15 prune floor
  4. STALE_BINARY    no build channel references a dead model id
  5. BACKUP_DELTA    a .bak is not massively larger than its live graph

Exit 0 = healthy, 1 = problem found. Read-only.

    python3 scripts/memory_health.py [--days 7] [--json]
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import subprocess
import sys
from collections import Counter
from datetime import datetime, timedelta, timezone

JCODE_HOME = pathlib.Path(os.environ.get("JCODE_HOME", pathlib.Path.home() / ".jcode"))
LOGS = JCODE_HOME / "logs"
MEMORY = JCODE_HOME / "memory"
BUILDS = JCODE_HOME / "builds"

PRUNE_FLOOR = 0.15  # prune_low_confidence() in memory_agent.rs
KNOWN_DEAD_MODEL_IDS = ["claude-haiku-4-5-20241022"]


class Result:
    def __init__(self) -> None:
        self.checks: list[dict] = []
        self.ok = True

    def add(self, name: str, ok: bool, detail: str, hint: str = "") -> None:
        self.checks.append({"check": name, "ok": ok, "detail": detail, "hint": hint})
        if not ok:
            self.ok = False


def _event_files(days: int) -> list[pathlib.Path]:
    if not LOGS.is_dir():
        return []
    cutoff = (datetime.now(timezone.utc) - timedelta(days=days)).date()
    out = []
    for p in sorted(LOGS.glob("memory-events-*.jsonl")):
        m = re.search(r"(\d{4}-\d{2}-\d{2})", p.name)
        if m and datetime.strptime(m.group(1), "%Y-%m-%d").date() >= cutoff:
            out.append(p)
    return out


def _events(days: int):
    for p in _event_files(days):
        for line in p.read_text(errors="replace").splitlines():
            line = line.strip()
            if not line:
                continue
            try:
                yield json.loads(line)
            except json.JSONDecodeError:
                continue


def _graphs() -> list[tuple[pathlib.Path, dict]]:
    if not MEMORY.is_dir():
        return []
    paths = [MEMORY / "global.json", *sorted((MEMORY / "projects").glob("*.json"))]
    out = []
    for p in paths:
        if not p.is_file() or ".bak-pre-restore" in p.name:
            continue
        try:
            out.append((p, json.loads(p.read_text())))
        except (json.JSONDecodeError, OSError):
            continue
    return out


def check_extraction(res: Result, days: int) -> None:
    """Errors pair 1:1 with PERIODIC extractions, so measure against those only.

    Counting `final_extraction_started` in the denominator dilutes the signal
    badly: during the real outage the periodic failure rate was a flat 100%
    (17 errors / 17 attempts on 2026-08-14), but blended against final
    extractions the same day reads as 30% and slips under a naive threshold.
    """
    counts = Counter()
    samples: list[str] = []
    for ev in _events(days):
        kind = ev.get("event")
        if kind in ("extraction_started", "final_extraction_started", "error"):
            counts[kind] += 1
        if kind == "error" and len(samples) < 3:
            samples.append(str(ev.get("detail", {}).get("message", ""))[:120])

    periodic = counts["extraction_started"]
    errors = counts["error"]
    if periodic == 0 and errors == 0:
        res.add("EXTRACTION", True, f"no periodic extraction activity in {days}d (idle)")
        return
    # Errors with no attempts at all still count as broken.
    rate = errors / periodic if periodic else 1.0
    ok = rate <= 0.10
    detail = f"{errors} errors / {periodic} periodic extractions ({rate:.0%}) over {days}d"
    if samples:
        detail += f" | e.g. {samples[0]}"
    res.add(
        "EXTRACTION",
        ok,
        detail,
        "Check the sidecar model id and provider login." if not ok else "",
    )


def check_writes(res: Result, days: int) -> None:
    """Attempts without writes is the exact silent-failure signature."""
    attempts = sum(
        1
        for ev in _events(days)
        if ev.get("event") in ("extraction_started", "final_extraction_started")
    )
    total = sum(len(g.get("memories", {})) for _, g in _graphs())
    if attempts == 0:
        res.add("WRITES", True, "no extraction attempts to compare against")
        return
    ok = total > 0
    res.add(
        "WRITES",
        ok,
        f"{attempts} extraction attempts over {days}d, {total} memories stored",
        "Extractions ran but nothing was stored - the classic silent failure."
        if not ok
        else "",
    )


def check_decay(res: Result, _days: int) -> None:
    doomed = []
    total = 0
    for path, graph in _graphs():
        for entry in graph.get("memories", {}).values():
            total += 1
            conf = entry.get("confidence")
            if isinstance(conf, (int, float)) and conf < PRUNE_FLOOR:
                doomed.append(f"{path.name}:{entry.get('content','')[:40]} conf={conf:.3f}")
    ok = not doomed
    detail = f"{len(doomed)}/{total} entries below the {PRUNE_FLOOR} prune floor"
    if doomed:
        detail += " | " + doomed[0]
    res.add(
        "DECAY",
        ok,
        detail,
        "These will be deleted once older than 24h. Decay without a judge verdict "
        "is the ADR-0021 bug; confirm the judge is reachable."
        if not ok
        else "",
    )


def check_stale_binary(res: Result, _days: int) -> None:
    if not BUILDS.is_dir():
        res.add("STALE_BINARY", True, "no builds dir")
        return
    bad = []
    checked = 0
    for channel in ("current", "stable", "shared-server"):
        binary = BUILDS / channel / "jcode"
        if not binary.exists():
            continue
        checked += 1
        try:
            blob = subprocess.run(
                ["strings", str(binary.resolve())],
                capture_output=True,
                text=True,
                timeout=120,
            ).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        for dead in KNOWN_DEAD_MODEL_IDS:
            if dead in blob:
                bad.append(f"{channel} -> {dead}")
    ok = not bad
    res.add(
        "STALE_BINARY",
        ok,
        f"{checked} channels checked; " + (", ".join(bad) if bad else "all clean"),
        "Repoint the channel symlink at the current build." if not ok else "",
    )


def check_backup_delta(res: Result, _days: int) -> None:
    """A .bak far larger than the live graph means entries vanished."""
    losses = []
    for path, graph in _graphs():
        bak = path.with_suffix(".bak")
        if not bak.is_file():
            continue
        try:
            baked = json.loads(bak.read_text())
        except (json.JSONDecodeError, OSError):
            continue
        live_ids = set(graph.get("memories", {}))
        bak_ids = set(baked.get("memories", {}))
        missing = bak_ids - live_ids
        if missing and len(missing) >= max(2, len(bak_ids) // 2):
            losses.append(f"{path.name}: {len(missing)} in .bak but not live")
    ok = not losses
    res.add(
        "BACKUP_DELTA",
        ok,
        ", ".join(losses) if losses else "no unexplained losses vs .bak",
        "Inspect the .bak; entries at confidence 0.0 were decayed away."
        if not ok
        else "",
    )


def check_decay_without_verdict(res: Result, days: int) -> None:
    """Detect the ADR-0021 signature directly in the event log.

    `maintenance_confidence {decayed: N}` immediately after a
    `judge_decision` whose decision is NOT `judge_ran` means memories lost
    confidence with no verdict behind it. This is the check that would have
    caught the 9-day wipe on day one.

    Note this fires on `cadence_carry` too, not just outright failures: a
    cadence-gated turn surfaces the carried set and marks everything else
    rejected. On this machine cadence_carry produced 45 of 90 observed decays.
    """
    last_decision: str | None = None
    offenders: Counter = Counter()
    total_bad = 0
    for ev in _events(days):
        kind = ev.get("event")
        if kind == "judge_decision":
            last_decision = ev.get("detail", {}).get("decision")
        elif kind == "maintenance_confidence":
            decayed = ev.get("detail", {}).get("decayed", 0) or 0
            if decayed and last_decision and last_decision != "judge_ran":
                offenders[last_decision] += decayed
                total_bad += decayed

    ok = total_bad == 0
    detail = (
        f"{total_bad} memories decayed with no judge verdict over {days}d"
        + (" | " + ", ".join(f"{k}:{v}" for k, v in offenders.most_common(3)) if offenders else "")
        if total_bad
        else f"no verdict-less decay in {days}d"
    )
    res.add(
        "DECAY_WITHOUT_VERDICT",
        ok,
        detail,
        "ADR-0021 regression, or a build predating the fix is still running "
        "(see RUNNING_BINARY)."
        if not ok
        else "",
    )


def check_gc_sanity(res: Result, days: int) -> None:
    """Garbage collection must retire before it deletes, and stay proportionate.

    GC (ADR-0022) is the only mechanism that removes a memory nobody asked to
    remove, and it fires on a wall-clock timer. Two failure shapes matter:
    deletions with no retirements before them (the two-stage guarantee broken),
    and a sweep eating a large share of the store at once.
    """
    retired = deleted = 0
    for ev in _events(days):
        if ev.get("event") == "maintenance_gc":
            d = ev.get("detail", {})
            retired += d.get("retired", 0) or 0
            deleted += d.get("deleted", 0) or 0

    total = sum(len(g.get("memories", {})) for _, g in _graphs())

    problems = []
    if deleted and not retired:
        problems.append(f"{deleted} deleted with 0 retired (two-stage GC bypassed)")
    if total and deleted > max(5, total // 2):
        problems.append(f"{deleted} deleted vs {total} stored (sweep too aggressive)")

    ok = not problems
    res.add(
        "GC_SANITY",
        ok,
        "; ".join(problems)
        if problems
        else f"{retired} retired, {deleted} deleted over {days}d ({total} stored)",
        "Check gc_action(): retirement must precede deletion by RETIRED_GRACE_DAYS."
        if not ok
        else "",
    )


def check_running_binary(res: Result, _days: int) -> None:
    """Long-lived processes keep the inode they started with.

    Repointing a channel symlink does NOT upgrade a running `jcode ... serve`
    or an open session; they hold the old binary until restarted. A memory fix
    can therefore be committed, built, installed AND still not be in effect,
    which reads exactly like "fixed" if you only inspect the symlinks.
    """
    try:
        ps = subprocess.run(
            ["ps", "-Ao", "pid=,command="], capture_output=True, text=True, timeout=30
        ).stdout
    except (OSError, subprocess.SubprocessError):
        res.add("RUNNING_BINARY", True, "could not enumerate processes")
        return

    want = (BUILDS / "current" / "jcode").resolve() if (BUILDS / "current" / "jcode").exists() else None
    if want is None:
        res.add("RUNNING_BINARY", True, "no current channel to compare against")
        return

    stale = []
    for line in ps.splitlines():
        line = line.strip()
        if "jcode" not in line:
            continue
        pid, _, cmd = line.partition(" ")
        if not pid.isdigit() or "memory_health" in cmd:
            continue
        try:
            lsof = subprocess.run(
                ["lsof", "-p", pid], capture_output=True, text=True, timeout=30
            ).stdout
        except (OSError, subprocess.SubprocessError):
            continue
        for l in lsof.splitlines():
            parts = l.split()
            if len(parts) > 4 and parts[3] == "txt" and "/builds/versions/" in l:
                running = pathlib.Path(parts[-1])
                if running != want:
                    stale.append(f"pid {pid} on {running.parent.name}")
                break

    ok = not stale
    res.add(
        "RUNNING_BINARY",
        ok,
        f"current={want.parent.name}; "
        + (f"{len(stale)} stale: {', '.join(stale[:3])}" if stale else "all live processes match"),
        "Restart these to pick up memory fixes; a symlink swap does not affect them."
        if not ok
        else "",
    )


CHECKS = (
    check_extraction,
    check_writes,
    check_decay,
    check_stale_binary,
    check_backup_delta,
    check_decay_without_verdict,
    check_gc_sanity,
    check_running_binary,
)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--days", type=int, default=7)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args()

    res = Result()
    for check in CHECKS:
        check(res, args.days)

    if args.json:
        print(json.dumps({"ok": res.ok, "checks": res.checks}, indent=2))
    else:
        for c in res.checks:
            print(f"[{'PASS' if c['ok'] else 'FAIL'}] {c['check']:<13} {c['detail']}")
            if c["hint"]:
                print(f"       -> {c['hint']}")
        print("\nmemory health:", "OK" if res.ok else "PROBLEMS FOUND")
    return 0 if res.ok else 1


if __name__ == "__main__":
    sys.exit(main())
