#!/usr/bin/env python3
"""Bounded, speculative flat-32 CFG from state_graph_trace artifacts.

Uses GNU objdump and the Python standard library. All branch arms and direct
calls are explored, regardless of trace counts. Indirect targets are observed
evidence only; a call's continuation is a may-return edge, not a return proof.
This is an offline inspection tool, never an idle-skip or safety proof.
"""
import argparse
from collections import Counter
import json
from pathlib import Path
import re

from wait_loops import disassemble


def direct(operands):
    match = re.fullmatch(r"(?:0x)?([0-9a-f]+)", operands.strip())
    return int(match[1], 16) if match else None


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("--low", type=lambda v: int(v, 0), required=True)
    parser.add_argument("--high", type=lambda v: int(v, 0), required=True)
    parser.add_argument("--limit", type=int, default=100000)
    args = parser.parse_args()
    trace = json.loads((args.directory / "trace.json").read_text())
    ram = args.directory / "extended.bin"
    decoded = {i.address: i for i in disassemble(
        ram, 0x100000, ("scope", args.low, args.high, 32))}
    observed = {n["pc"]: n for n in trace["nodes"]
                if n["bits"] == 32 and args.low <= n["pc"] < args.high}
    memory = ram.read_bytes()
    pending = list(sorted(observed, reverse=True))
    nodes, edges, unresolved = {}, [], []
    repairs = 0
    while pending:
        pc = pending.pop()
        if pc in nodes:
            continue
        if len(nodes) >= args.limit:
            raise RuntimeError("graph node bound exceeded; no complete graph written")
        if pc not in decoded:
            if repairs >= 512:
                raise RuntimeError("disassembly repair bound exceeded")
            repairs += 1
            decoded.update({i.address: i for i in disassemble(
                ram, 0x100000, ("entry", pc, min(pc + 256, args.high), 32))})
        ins = decoded.get(pc)
        if ins is None or ins.mnemonic in ("(bad)", ".byte"):
            unresolved.append({"pc": pc, "reason": "decode"})
            nodes[pc] = {"pc": pc, "instruction": "unknown", "count": 0}
            continue
        runtime = observed.get(pc, {})
        nodes[pc] = {"pc": pc, "size": ins.size,
                     "instruction": f"{ins.mnemonic} {ins.operands}".strip(),
                     "count": runtime.get("count", 0)}
        if runtime and (runtime.get("changed") or bytes(runtime["bytes"]) !=
                        memory[pc - 0x100000:pc - 0x100000 + 15]):
            unresolved.append({"pc": pc, "reason": "15-byte code window changed or differs from snapshot"})
            continue
        name, operand = ins.mnemonic, ins.operands
        target = direct(operand)
        successors = []
        following = pc + ins.size
        if name in ("ret", "retf", "iret", "iretd"):
            pass  # Do not conflate separate callers by adding observed RET edges.
        elif name in ("call", "jmp"):
            if target is not None:
                successors.append((target, "direct-call" if name == "call" else "jump"))
            else:
                unresolved.append({"pc": pc, "reason": "open indirect target set"})
                for dst in runtime.get("successors", {}):
                    successors.append((int(dst), "observed-indirect"))
            if name == "call":
                successors.append((following, "may-return"))
        elif name.startswith(("j", "loop")):
            if target is None:
                unresolved.append({"pc": pc, "reason": "unknown branch target"})
            else:
                successors.append((target, "branch"))
                successors.append((following, "fallthrough"))
        elif name in ("int", "int3", "into", "lcall", "ljmp", "hlt", "ud2"):
            unresolved.append({"pc": pc, "reason": "service/far transfer/stop"})
            if name in ("int", "int3", "into", "lcall"):
                successors.append((following, "may-return-service"))
        else:
            successors.append((following, "next"))
        for dst, kind in successors:
            inside = args.low <= dst < args.high
            count = runtime.get("successors", {}).get(str(dst), 0)
            edges.append({"source": pc, "target": dst, "kind": kind,
                          "observed": count, "inside_scope": inside})
            if inside:
                pending.append(dst)
            else:
                unresolved.append({"pc": pc, "target": dst, "reason": "outside scope"})
    # Overlapping decodes are legitimate x86 but require manual/context review.
    occupied, overlaps = {}, set()
    for pc, n in nodes.items():
        for byte in range(pc, pc + n.get("size", 0)):
            if byte in occupied:
                overlaps.update((pc, occupied[byte]))
            occupied[byte] = pc
    summary = {
        "scope": [args.low, args.high], "observed_roots": len(observed),
        "nodes": len(nodes), "unexecuted_nodes": sum(n["count"] == 0 for n in nodes.values()),
        "edges": len(edges), "unexecuted_conditional_edges": sum(
            e["kind"] in ("branch", "fallthrough") and not e["observed"] for e in edges),
        "unresolved": dict(Counter(n["reason"] for n in unresolved)),
        "overlapping_nodes": len(overlaps), "decode_repairs": repairs,
        "method": "syntactic over-approximation; no path feasibility or callee purity proof",
    }
    (args.directory / "graph.json").write_text(json.dumps({
        "summary": summary, "nodes": list(nodes.values()), "edges": edges,
        "unresolved": unresolved, "overlapping_nodes": sorted(overlaps)}, indent=2) + "\n")
    (args.directory / "graph.asm").write_text("".join(
        f'{pc:08x} {n["count"]:10d}  {n["instruction"]}\n'
        for pc, n in sorted(nodes.items())))
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
