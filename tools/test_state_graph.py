"""Small executable fixtures test the audit's evidence boundaries."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


class SpeculativeGraphTests(unittest.TestCase):
    def graph(self, code, roots):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp)
            memory = code + bytes(32)
            (path / "extended.bin").write_bytes(memory)
            nodes = []
            for offset, successors in roots:
                nodes.append({"pc": 0x100000 + offset, "bits": 32, "count": 1,
                              "bytes": list(memory[offset:offset + 15]),
                              "changed": False,
                              "successors": {str(0x100000 + dst): 1 for dst in successors}})
            (path / "trace.json").write_text(json.dumps({"nodes": nodes}))
            subprocess.run([sys.executable, str(Path(__file__).with_name("state_graph.py")),
                            str(path), "--low", "0x100000", "--high", hex(0x100000 + len(code))],
                           check=True, capture_output=True, text=True)
            return json.loads((path / "graph.json").read_text())

    def test_expands_unexecuted_arm_and_stops_at_returns(self):
        # JNE cold; MOV AL,1; RET; cold: MOV AL,2; RET; unreachable: INT3
        graph = self.graph(bytes.fromhex("75 03 b0 01 c3 b0 02 c3 cc"), [(0, [2])])
        pcs = {n["pc"] for n in graph["nodes"]}
        self.assertEqual(pcs, {0x100000, 0x100002, 0x100004, 0x100005, 0x100007})
        cold_edge = next(e for e in graph["edges"] if e["target"] == 0x100005)
        self.assertEqual(cold_edge["observed"], 0)

    def test_observed_indirect_target_does_not_close_target_set(self):
        # CALL EAX; RET; observed callee: RET
        graph = self.graph(bytes.fromhex("ff d0 c3 c3"), [(0, [3])])
        self.assertEqual(graph["summary"]["unresolved"]["open indirect target set"], 1)
        edges = {(e["target"], e["kind"]) for e in graph["edges"]}
        self.assertIn((0x100003, "observed-indirect"), edges)
        self.assertIn((0x100002, "may-return"), edges)

    def test_unknown_indirect_jump_has_no_invented_fallthrough(self):
        graph = self.graph(bytes.fromhex("ff e0 cc"), [(0, [])])
        self.assertEqual(len(graph["nodes"]), 1)
        self.assertEqual(graph["edges"], [])
        self.assertEqual(graph["summary"]["unresolved"]["open indirect target set"], 1)


if __name__ == "__main__":
    unittest.main()
