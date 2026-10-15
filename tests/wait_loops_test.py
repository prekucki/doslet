"""Original fixtures for the host-only wait inventory; stdlib and objdump."""
import json
from pathlib import Path
import struct
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'tools'))
import wait_loops


class WaitInventoryTest(unittest.TestCase):
    def test_a_fixed_flag_poll_differs_from_a_pointer_walk(self):
        fixed = [wait_loops.Instruction(0, 7, 'cmp', 'BYTE PTR ds:0x200010,0x0'),
                 wait_loops.Instruction(7, 2, 'je', '0x0')]
        walk = [wait_loops.Instruction(0, 3, 'cmp', 'BYTE PTR [esi],0x0'),
                wait_loops.Instruction(3, 1, 'inc', 'esi'),
                wait_loops.Instruction(4, 2, 'jne', '0x0')]
        self.assertEqual(wait_loops.category(fixed), 'fixed_memory_candidate')
        self.assertEqual(wait_loops.category(walk), 'memory_work')

    def test_merging_traces_retains_target_versions_and_port_bit_ranges(self):
        left = dict(steps=10, branches=[dict(source=10,target=2,bits=32,taken=3,fallthrough=1)],
                    ports=[dict(source=2,port=0x3da,reads=4,**{'and':1,'or':9})])
        right = dict(steps=20, branches=[dict(source=10,target=2,bits=32,taken=4,fallthrough=2),
                                        dict(source=10,target=4,bits=32,taken=1,fallthrough=0)],
                     ports=[dict(source=2,port=0x3da,reads=5,**{'and':0,'or':8})])
        merged = wait_loops.merge_traces([left, right])
        self.assertEqual(merged['steps'], 30)
        self.assertEqual(len(merged['branches']), 2)
        self.assertEqual(merged['branches'][0]['taken'], 7)
        self.assertEqual(merged['ports'][0]['reads'], 9)
        self.assertEqual(merged['ports'][0]['and'], 0)
        self.assertEqual(merged['ports'][0]['or'], 9)
    def test_a_polling_cycle_is_not_confused_with_a_backward_tail_jump(self):
        with tempfile.TemporaryDirectory(prefix='doslet-wait-test-') as directory:
            path = Path(directory) / 'code.bin'
            # IN; TEST; JZ back; RET; JMP back to the function entry.
            path.write_bytes(bytes.fromhex('ec a808 74fb c3 ebf8'))
            trace = dict(branches=[dict(source=0x1003, target=0x1000, bits=32,
                                       taken=7, fallthrough=1)],
                         ports=[dict(source=0x1000, port=0x3da)])
            rows = wait_loops.scan(path, 0x1000, [('fixture', 0x1000, 0x1008, 32)], trace)
            poll = next(row for row in rows if row['branch'] == 0x1003)
            tail = next(row for row in rows if row['branch'] == 0x1006)
            self.assertEqual(poll['category'], 'port_poll')
            self.assertTrue(poll['cycle_proven'])
            self.assertEqual(poll['taken'], 7)
            self.assertIn('0x03da (observed)', poll['port_hints'])
            self.assertEqual(tail['category'], 'backward_transfer')
            self.assertFalse(tail['cycle_proven'])
            self.assertEqual(json.loads(json.dumps(rows)), rows)

    def test_only_executable_le_objects_are_scanned(self):
        with tempfile.TemporaryDirectory(prefix='doslet-wait-test-') as directory:
            path = Path(directory) / 'original.exe'
            data = bytearray(0x200)
            header = 0x40
            data[header:header+4] = b'LE\0\0'
            struct.pack_into('<H', data, header + 8, 2)
            struct.pack_into('<II', data, header + 0x40, 0xc4, 2)
            struct.pack_into('<IIIIII', data, header + 0xc4, 16, 0x10000, 0x2005, 1, 1, 0)
            struct.pack_into('<IIIIII', data, header + 0xdc, 32, 0xc0000, 0x2003, 2, 1, 0)
            path.write_bytes(data)
            regions = wait_loops.executable_regions(path, 0x18e000)
            self.assertEqual(regions, [('original.exe:LE-object-1', 0x19e000, 0x19e010, 32)])


if __name__ == '__main__':
    unittest.main()
