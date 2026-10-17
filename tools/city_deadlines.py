#!/usr/bin/env python3
"""Timing audit for the documented Heroes city fixture, not a runtime recipe."""
import argparse
from bisect import bisect_right
from collections import Counter, defaultdict, deque
import json
import math
from pathlib import Path
from statistics import median


def stats(values):
    if not values:
        return {"count": 0}
    values = sorted(value / 1e6 for value in values)
    return {"count": len(values), "min_ms": values[0],
            "median_ms": median(values),
            "p95_ms": values[math.ceil(len(values)*.95)-1], "max_ms": values[-1]}


def analyze(directory):
    data = json.loads((directory / "probes.json").read_text())
    t0 = data["start_ns"]
    memory_index = {address: i for i, (address, length) in enumerate(data["memory_ranges"])}
    def counter(event):
        return int.from_bytes(bytes(event["memory_after"][memory_index[0x281094]][:4]), "little")
    pending = defaultdict(deque)
    callbacks, callback_stack, callback_waits = [], [], defaultdict(list)
    due = None
    active = None
    episodes, ticks, resets = [], [], []
    skip_contexts = Counter()
    timer_entries = 0
    zero_counter_ticks = 0
    vga = []
    vga_active = []
    releases, acks, mixes, copies = [], [], [], []
    mix_active, copy_active = [], []
    service_due, service_active = None, None
    services, overwritten_flags = [], 0
    unpaired_callbacks = 0
    for event in data["events"]:
        before, after, pc = event["before_ns"], event["after_ns"], event["pc"]
        regs = event["registers"]
        if event["dma_block"]:
            releases.append(after)
        if not event["probe"]:
            continue
        if pc == 0x219F90:
            ticks.append(before)
            zero_counter_ticks += counter(event) == 0
        elif pc == 0x219FAB and regs[0] == 0 and counter(event) == 0:
            if due is not None:
                raise ValueError("unconsumed animation release")
            due = after
        elif pc == 0x1F41D1:
            if active is not None:
                raise ValueError("nested animation episode")
            active = {"due_ns": due, "start_ns": before}
            due = None
        elif pc == 0x1F41E9:
            if active is None or counter(event) != 10:
                raise ValueError("unmatched animation completion/reset")
            active["reset_ns"] = after
            episodes.append(active)
            resets.append(after)
            active = None
        elif pc == 0x22F006:
            timer_entries += 1
            if event["next"] == 0x22F11D:
                context = hex(callback_stack[-1][1]) if callback_stack else "outside callback"
                if vga_active:
                    context += "/VGA"
                skip_contexts[context] += 1
        elif pc == 0x22F06D:
            pending[regs[7] // 4].append(after)
        elif pc == 0x22F0AC:
            slot = regs[7] // 4
            if pending[slot]:
                callback_waits[slot].append(before - pending[slot].popleft())
            else:
                unpaired_callbacks += 1
            callback_stack.append((slot, event["next"], before))
        elif pc == 0x22F0B2:
            if not callback_stack:
                raise ValueError("unmatched timer callback return")
            slot, target, start = callback_stack.pop()
            if slot != regs[7] // 4:
                raise ValueError("callback slot changed across call")
            callbacks.append({"slot": slot, "target": target, "start_ns": start, "end_ns": before})
        elif pc == 0x22426A:
            vga_active.append(before)
        elif pc == 0x224283:
            vga.append((vga_active.pop(), before))
        elif pc == 0x181E5:
            acks.append(after)
        elif pc == 0x23D9E5 and event["next"] == 0x23D356:
            mix_active.append(before)
        elif pc == 0x23D38E:
            mixes.append((mix_active.pop(), before))
        elif pc == 0x23DB2F and event["next"] == 0x23BE81:
            copy_active.append((before, regs[7]))
        elif pc == 0x23BE9F:
            start, destination = copy_active.pop()
            copies.append((start, before, destination))
        elif pc == 0x219F6E:
            if service_due is None:
                service_due = after
            else:
                overwritten_flags += 1
        elif pc == 0x21AC45:
            if service_active is None:
                service_active = (service_due, before)
        elif pc == 0x21B422:
            if service_active is not None:
                release, start = service_active
                services.append({"due_ns": release, "start_ns": start, "end_ns": after})
            service_active, service_due = None, None
    for episode in episodes:
        release = episode["due_ns"]
        if release is not None:
            idx = bisect_right(ticks, release)
            episode["next_tick_ns"] = ticks[idx] if idx < len(ticks) else None
    completed = [e for e in episodes if e["due_ns"] is not None]
    margins = [e["next_tick_ns"] - e["reset_ns"] for e in completed if e["next_tick_ns"] is not None]
    # Match observed service to a block interval, not blindly to ordinal indices.
    block_windows = []
    for start, end in zip(releases, releases[1:]):
        ack = next((v for v in acks if start <= v < end), None)
        copied = [v for v in copies if start <= v[1] < end]
        block_windows.append({"release_ns": start, "next_ns": end,
                              "ack_ns": ack, "copy_completions": copied})
    summary = {
        "mhz": json.loads((directory / "metrics.json").read_text())["mhz"],
        "animation": {
            "episodes": len(episodes), "unfinished": active is not None,
            "release_to_start": stats([e["start_ns"]-e["due_ns"] for e in completed]),
            "work_to_reset": stats([e["reset_ns"]-e["start_ns"] for e in episodes]),
            "start_period": stats([b["start_ns"]-a["start_ns"] for a,b in zip(episodes,episodes[1:])]),
            "reset_to_next_release": stats([b["due_ns"]-a["reset_ns"] for a,b in zip(episodes,episodes[1:]) if b["due_ns"] is not None]),
            "margin_to_next_counter_tick": stats(margins),
            "resets_after_next_tick": sum(v < 0 for v in margins),
        },
        "timer": {"entries": timer_entries, "skipped_reentry": sum(skip_contexts.values()),
                  "skipped_by_callback": dict(skip_contexts), "counter_callbacks": len(ticks),
                  "ticks_with_animation_counter_already_zero": zero_counter_ticks,
                  "counter_intervals": stats([b-a for a,b in zip(ticks,ticks[1:])]),
                  "queue_delay_by_slot": {k: stats(v) for k,v in callback_waits.items()},
                  "callback_duration_by_target": {
                      hex(target): stats([c["end_ns"]-c["start_ns"] for c in callbacks if c["target"] == target])
                      for target in sorted({c["target"] for c in callbacks})},
                  "unpaired_callback_starts": unpaired_callbacks,
                  "unfinished_callbacks": len(callback_stack)},
        "vga_wait": stats([b-a for a,b in vga]),
        "audio": {
            "blocks": len(releases), "acknowledgements": len(acks),
            "block_intervals": stats([b-a for a,b in zip(releases,releases[1:])]),
            "ack_latency": stats([w["ack_ns"]-w["release_ns"] for w in block_windows if w["ack_ns"] is not None]),
            "block_windows_without_copy_completion": sum(not w["copy_completions"] for w in block_windows),
            "copy_completion_latency": stats([v[1]-w["release_ns"] for w in block_windows for v in w["copy_completions"]]),
            "mix_duration": stats([b-a for a,b in mixes]),
            "copy_duration": stats([b-a for a,b,d in copies]),
            "copy_destinations": dict(Counter(hex(d) for a,b,d in copies)),
            "service_flag_to_start": stats([s["start_ns"]-s["due_ns"] for s in services if s["due_ns"] is not None]),
            "service_duration": stats([s["end_ns"]-s["start_ns"] for s in services]),
            "overwritten_service_flags": overwritten_flags,
        },
    }
    detail = {"start_ns": t0, "animation": episodes, "callbacks": callbacks,
              "vga_waits": vga, "audio_blocks": block_windows, "audio_services": services,
              "mixes": mixes, "copies": copies, "summary": summary}
    (directory / "deadlines.json").write_text(json.dumps(detail, indent=2) + "\n")
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    args = parser.parse_args()
    rows = [analyze(path.parent) for path in args.directory.glob("*/probes.json")]
    rows.sort(key=lambda row: row["mhz"])
    (args.directory / "deadlines-summary.json").write_text(json.dumps(rows, indent=2) + "\n")
    print(json.dumps(rows, indent=2))


if __name__ == "__main__":
    main()
