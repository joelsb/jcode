#!/usr/bin/env python3
"""Meta-test for scripts/memory_health.py.

A checker that has never failed proves nothing. This plants known defects into
a synthetic JCODE_HOME and requires memory_health to catch each one, plus
requires a clean home to come back clean (no false alarms).

Defect 8 is the important one: it replays the REAL 2026-08-14 event mix, where
17/17 periodic extractions failed but blending in 39 successful final
extractions makes the blended rate look like a tolerable 30%. An earlier draft
of the checker used that blended denominator and passed the outage.

    python3 scripts/test_memory_health.py
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone

HERE = pathlib.Path(__file__).resolve().parent
CHECKER = HERE / "memory_health.py"


# ---------------------------------------------------------------- fixture ---


def _iso(days_ago: int = 0) -> str:
    return (datetime.now(timezone.utc) - timedelta(days=days_ago)).isoformat()


def _memory(content: str, confidence: float = 0.6, age_days: int = 3) -> dict:
    return {
        "id": f"mem_{abs(hash(content))}",
        "category": "fact",
        "content": content,
        "confidence": confidence,
        "created_at": _iso(age_days),
        "active": True,
    }


def build_home(root: pathlib.Path) -> pathlib.Path:
    """A healthy JCODE_HOME: extractions succeed, memories stored, nothing decayed."""
    home = root / "jcode-home"
    (home / "logs").mkdir(parents=True)
    (home / "memory" / "projects").mkdir(parents=True)

    day = (datetime.now(timezone.utc)).strftime("%Y-%m-%d")
    events = []
    for _ in range(10):
        events.append({"timestamp": _iso(0), "event": "extraction_started",
                       "detail": {"reason": "periodic"}})
        events.append({"timestamp": _iso(0), "event": "embedding_complete",
                       "detail": {"hits": 2}})
    (home / "logs" / f"memory-events-{day}.jsonl").write_text(
        "\n".join(json.dumps(e) for e in events) + "\n"
    )

    entries = [_memory("alpha fact"), _memory("beta fact")]
    graph = {
        "graph_version": 2,
        "memories": {e["id"]: e for e in entries},
        "tags": {}, "clusters": {}, "edges": {}, "reverse_edges": {},
    }
    (home / "memory" / "global.json").write_text(json.dumps(graph))
    shutil.copy(home / "memory" / "global.json", home / "memory" / "global.bak")
    return home


def run_checker(home: pathlib.Path, days: int = 7) -> tuple[int, dict]:
    env = dict(os.environ, JCODE_HOME=str(home))
    proc = subprocess.run(
        [sys.executable, str(CHECKER), "--days", str(days), "--json"],
        capture_output=True, text=True, env=env, timeout=300,
    )
    try:
        return proc.returncode, json.loads(proc.stdout)
    except json.JSONDecodeError:
        raise AssertionError(f"checker produced no JSON:\n{proc.stdout}\n{proc.stderr}")


def failed_checks(report: dict) -> set[str]:
    return {c["check"] for c in report["checks"] if not c["ok"]}


# ----------------------------------------------------------------- defects ---
# Each returns the check name it MUST trigger.


def defect_all_periodic_extractions_404(home: pathlib.Path) -> str:
    """The real outage: every periodic extraction errors."""
    day = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    path = home / "logs" / f"memory-events-{day}.jsonl"
    lines = [
        json.dumps({"timestamp": _iso(0), "event": "extraction_started",
                    "detail": {"reason": "periodic"}}),
        json.dumps({"timestamp": _iso(0), "event": "error", "detail": {
            "message": 'Claude API error (404 Not Found): model: claude-haiku-4-5-20241022'}}),
    ] * 17
    path.write_text("\n".join(lines) + "\n")
    return "EXTRACTION"


def defect_blended_rate_hides_outage(home: pathlib.Path) -> str:
    """REAL 2026-08-14 mix: 17/17 periodic fail, 39 final succeed.

    Blended that is 17/56 = 30% and looks survivable. Against periodic alone it
    is 100%. This defect exists to kill the diluted denominator specifically.
    """
    day = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    lines = []
    for _ in range(17):
        lines.append(json.dumps({"timestamp": _iso(0), "event": "extraction_started",
                                 "detail": {"reason": "periodic"}}))
        lines.append(json.dumps({"timestamp": _iso(0), "event": "error", "detail": {
            "message": "Claude API error (404 Not Found): model: claude-haiku-4-5-20241022"}}))
    for _ in range(39):
        lines.append(json.dumps({"timestamp": _iso(0),
                                 "event": "final_extraction_started",
                                 "detail": {"transcript_chars": 5000}}))
    (home / "logs" / f"memory-events-{day}.jsonl").write_text("\n".join(lines) + "\n")
    return "EXTRACTION"


def defect_extractions_but_no_memories(home: pathlib.Path) -> str:
    """Pipeline runs, nothing is ever written - the silent failure."""
    for name in ("global.json", "global.bak"):
        p = home / "memory" / name
        graph = json.loads(p.read_text())
        graph["memories"] = {}
        p.write_text(json.dumps(graph))
    return "WRITES"


def defect_entry_below_prune_floor(home: pathlib.Path) -> str:
    """Decayed to 0.0 - what the judge-failure bug produced."""
    p = home / "memory" / "global.json"
    graph = json.loads(p.read_text())
    victim = next(iter(graph["memories"].values()))
    victim["confidence"] = 0.0
    p.write_text(json.dumps(graph))
    return "DECAY"


def defect_stale_model_id_in_binary(home: pathlib.Path) -> str:
    """A build channel still shipping a dead model id."""
    channel = home / "builds" / "shared-server"
    channel.mkdir(parents=True)
    binary = channel / "jcode"
    binary.write_text("padding\nclaude-haiku-4-5-20241022\npadding\n")
    binary.chmod(0o755)
    return "STALE_BINARY"


def defect_memories_vanished_vs_backup(home: pathlib.Path) -> str:
    """Live graph emptied while .bak still holds the entries."""
    p = home / "memory" / "global.json"
    graph = json.loads(p.read_text())
    graph["memories"] = {}
    p.write_text(json.dumps(graph))
    # keep global.bak intact => live is missing everything the bak has
    return "BACKUP_DELTA"


def defect_project_graph_decayed(home: pathlib.Path) -> str:
    """Decay in a project graph, not just global."""
    entry = _memory("project fact", confidence=0.02)
    graph = {"graph_version": 2, "memories": {entry["id"]: entry},
             "tags": {}, "clusters": {}, "edges": {}, "reverse_edges": {}}
    (home / "memory" / "projects" / "abc123.json").write_text(json.dumps(graph))
    return "DECAY"


def defect_errors_with_zero_attempts(home: pathlib.Path) -> str:
    """Errors logged while no attempt was recorded - must not divide by zero into a pass."""
    day = datetime.now(timezone.utc).strftime("%Y-%m-%d")
    lines = [json.dumps({"timestamp": _iso(0), "event": "error",
                         "detail": {"message": "sidecar unreachable"}})] * 5
    (home / "logs" / f"memory-events-{day}.jsonl").write_text("\n".join(lines) + "\n")
    return "EXTRACTION"


DEFECTS = [
    defect_all_periodic_extractions_404,
    defect_blended_rate_hides_outage,
    defect_extractions_but_no_memories,
    defect_entry_below_prune_floor,
    defect_stale_model_id_in_binary,
    defect_memories_vanished_vs_backup,
    defect_project_graph_decayed,
    defect_errors_with_zero_attempts,
]


# -------------------------------------------------------------------- main ---


def main() -> int:
    failures: list[str] = []

    # 0. A clean home must pass. Guards against a checker that just always fails.
    with tempfile.TemporaryDirectory() as tmp:
        home = build_home(pathlib.Path(tmp))
        code, report = run_checker(home)
        if code != 0:
            failures.append(f"clean home reported unhealthy: {failed_checks(report)}")
            print(f"[FAIL] clean home should pass, got {failed_checks(report)}")
        else:
            print("[PASS] clean home reports healthy")

    # 1..N. Each planted defect must be caught by its named check.
    for defect in DEFECTS:
        with tempfile.TemporaryDirectory() as tmp:
            home = build_home(pathlib.Path(tmp))
            expected = defect(home)
            code, report = run_checker(home)
            caught = failed_checks(report)
            if code == 0 or expected not in caught:
                failures.append(f"{defect.__name__}: expected {expected}, caught {caught or 'nothing'}")
                print(f"[FAIL] {defect.__name__}: expected {expected}, caught {caught or 'nothing'}")
            else:
                print(f"[PASS] {defect.__name__} -> {expected}")

    print()
    total = len(DEFECTS) + 1
    print(f"{total - len(failures)}/{total} meta-checks passed")
    if failures:
        print("\nthe health checker does NOT catch:")
        for f in failures:
            print(f"  - {f}")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
