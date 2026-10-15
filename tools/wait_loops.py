#!/usr/bin/env python3
"""Inventory backward branches and HLT in guest code; never patch or accelerate.

Uses stdlib Python and GNU objdump. Linear disassembly can include inline data;
runtime trace confirms executed edges but only for the recorded scenario.
"""
import argparse
import bisect
from collections import Counter
from dataclasses import dataclass
import json
from pathlib import Path
import re
import struct
import subprocess


@dataclass
class Instruction:
    address: int
    size: int
    mnemonic: str
    operands: str


def executable_regions(path, bias):
    """Find valid nested LE headers and executable objects, not data objects."""
    data = path.read_bytes()
    regions = []
    start = 0
    while (header := data.find(b'LE\0\0', start)) >= 0:
        start = header + 4
        if header + 0x88 > len(data):
            continue
        cpu = struct.unpack_from('<H', data, header + 8)[0]
        table, count = struct.unpack_from('<II', data, header + 0x40)
        if cpu not in (2, 3) or not 0 < count <= 256 or header + table + count * 24 > len(data):
            continue
        for index in range(count):
            size, base, flags = struct.unpack_from('<III', data, header + table + index * 24)
            if flags & 4 and size:
                regions.append((f'{path.name}:LE-object-{index+1}', base + bias, base + bias + size,
                                32 if flags & 0x2000 else 16))
    if not regions:
        raise ValueError('No valid LE executable objects; provide explicit --region instead')
    return regions


def disassemble(path, ram_base, region):
    _, low, high, bits = region
    size = path.stat().st_size
    if low < ram_base or high > ram_base + size or low >= high:
        raise ValueError(f'Region outside snapshot: {region}')
    result = subprocess.run(['objdump', '-D', '-b', 'binary', '-m', 'i386',
                             '-M', 'intel' + (',addr16,data16' if bits == 16 else ''),
                             '--insn-width=16', f'--adjust-vma={ram_base}',
                             f'--start-address={low}', f'--stop-address={high}', str(path)],
                            check=True, capture_output=True, text=True)
    instructions = []
    pattern = re.compile(r'^\s*([0-9a-f]+):\s*((?:[0-9a-f]{2}\s+)+)\s*(\S+)(?:\s+(.*))?$', re.I)
    for line in result.stdout.splitlines():
        match = pattern.match(line)
        if not match:
            continue
        address, raw, mnemonic, operands = match.groups()
        operands = operands or ''
        while mnemonic in ('data16','data32','addr16','addr32','lock','rep','repz','repnz') and operands:
            parts = operands.split(None, 1)
            mnemonic, operands = parts[0], parts[1] if len(parts) > 1 else ''
        instructions.append(Instruction(int(address, 16), len(raw.split()), mnemonic, operands))
    return instructions


def target(instruction):
    if not instruction.mnemonic.startswith(('j', 'loop')):
        return None
    match = re.match(r'(?:0x)?([0-9a-f]+)(?:\s|$)', instruction.operands)
    return int(match.group(1), 16) if match else None


def memory_operand(instruction):
    return '[' in instruction.operands or 'ptr ds:' in instruction.operands.lower()


def fixed_memory_reads(body):
    registers = {}
    for parent, aliases in [('eax', ('eax','ax','al','ah')), ('ebx', ('ebx','bx','bl','bh')),
                            ('ecx', ('ecx','cx','cl','ch')), ('edx', ('edx','dx','dl','dh')),
                            ('esi', ('esi','si')), ('edi', ('edi','di')),
                            ('ebp', ('ebp','bp')), ('esp', ('esp','sp'))]:
        registers.update((alias, parent) for alias in aliases)
    changed = set()
    read_addresses = set()
    for instruction in body:
        name, operands = instruction.mnemonic, instruction.operands.lower()
        if name.startswith('j') or name in ('cmp', 'test', 'bt', 'nop'):
            pass
        elif name in ('mov', 'movzx', 'movsx', 'and', 'or', 'xor'):
            destination = operands.split(',')[0].strip()
            if destination not in registers:
                return False  # A memory write or an unhandled implicit effect.
            changed.add(registers[destination])
        else:
            return False
        if memory_operand(instruction):
            for expression in re.findall(r'\[([^]]+)\]', operands):
                for token in re.findall(r'\b[a-z][a-z0-9]*\b', expression):
                    if token in registers:
                        read_addresses.add(registers[token])
    return not bool(changed & read_addresses)


def category(body):
    names = [instruction.mnemonic for instruction in body]
    if 'in' in names:
        return 'port_poll'
    if 'int' in names:
        return 'interrupt_poll'
    if any(name.startswith(('call', 'lcall')) for name in names):
        return 'calls_or_events'
    if any(memory_operand(i) and i.mnemonic.startswith(('cmp', 'test', 'mov')) for i in body):
        return 'fixed_memory_candidate' if fixed_memory_reads(body) else 'memory_work'
    if any(name.startswith(('loop', 'dec', 'inc')) for name in names):
        return 'counter_or_delay'
    return 'back_edge'


def reaches_branch(instructions, addresses, start, end):
    """A backward transfer is a loop only if its target can reach the branch.

    Calls use their return continuation. Indirect jumps remain unresolved;
    loops reached solely through them are retained as unproven candidates.
    """
    pending = [start]
    visited = set()
    unresolved = False
    reached = False
    while pending:
        index = pending.pop()
        if index == end:
            visited.add(index)
            reached = True
            continue
        if index in visited:
            continue
        visited.add(index)
        instruction = instructions[index]
        if instruction.mnemonic.startswith(('ret', 'iret')) or instruction.mnemonic in ('(bad)', '.byte'):
            continue
        destination = target(instruction)
        if instruction.mnemonic.startswith(('j', 'loop')):
            if destination is None:
                unresolved = True
            else:
                branch = bisect.bisect_left(addresses, destination)
                if start <= branch <= end and addresses[branch] == destination:
                    pending.append(branch)
            if instruction.mnemonic == 'jmp':
                continue
        if index + 1 <= end and instruction.address + instruction.size == addresses[index + 1]:
            pending.append(index + 1)
    return (True if reached else None if unresolved else False), visited


def port_hints(instructions, start, end, trace_ports, reachable):
    hints = set()
    for index in range(start, end + 1):
        if index not in reachable:
            continue
        instruction = instructions[index]
        if instruction.mnemonic != 'in':
            continue
        for port in trace_ports.get(instruction.address, []):
            hints.add(f'0x{port["port"]:04x} (observed)')
        value = instruction.operands.split(',')[-1].strip()
        if re.fullmatch(r'0x[0-9a-f]+', value):
            hints.add(f'{value} (immediate)')
        elif value == 'dx':
            # A nearby constant is a static hint, not a proven runtime register.
            for previous in reversed(instructions[max(0, start - 32):index]):
                if previous.mnemonic.startswith(('ret', 'call', 'jmp')):
                    break
                match = re.match(r'(?:e?dx),\s*(0x[0-9a-f]+)$', previous.operands)
                if previous.mnemonic == 'mov' and match:
                    hints.add(f'{match.group(1)} (nearby constant)')
                    break
                if previous.operands.split(',')[0].strip() in ('dx', 'edx'):
                    break
    return sorted(hints)


def scan(path, ram_base, regions, trace):
    edges = {(row['source'], row['target'], row['bits']): row for row in trace.get('branches', [])}
    ports = {}
    halts = {row['source']: row['calls'] for row in trace.get('halts', [])}
    for row in trace.get('ports', []):
        ports.setdefault(row['source'], []).append(row)
    rows = []
    for region in regions:
        instructions = disassemble(path, ram_base, region)
        addresses = [instruction.address for instruction in instructions]
        for end, instruction in enumerate(instructions):
            destination = target(instruction)
            if instruction.mnemonic == 'hlt':
                rows.append(dict(region=region[0], bits=region[3], start=instruction.address,
                                 branch=instruction.address, category='halt', instructions=1,
                                 port_hints=[], taken=halts.get(instruction.address, 0), observed=instruction.address in halts, memory_writes=False,
                                 has_calls=False, uncertain_decode=False, excerpt=['hlt']))
                continue
            if destination is None or destination > instruction.address:
                continue
            start = bisect.bisect_left(addresses, destination)
            if start > end or addresses[start] != destination:
                continue
            cycle, reachable = reaches_branch(instructions, addresses, start, end)
            body = [instructions[index] for index in sorted(reachable)]
            observed = edges.get((instruction.address, destination, region[3]), {})
            writes = any(memory_operand(i) and i.mnemonic not in ('cmp', 'test', 'lea', 'call', 'jmp')
                         and ('[' in i.operands.split(',')[0] or 'ptr' in i.operands.split(',')[0].lower()) for i in body)
            rows.append(dict(region=region[0], bits=region[3], start=destination,
                             branch=instruction.address, category=category(body) if cycle is not False else 'backward_transfer', instructions=len(body),
                             port_hints=port_hints(instructions, start, end, ports, reachable),
                             taken=observed.get('taken', 0), fallthrough=observed.get('fallthrough', 0),
                             observed=bool(observed), memory_writes=writes,
                             cycle_proven=cycle,
                             has_calls=any(i.mnemonic.startswith('call') for i in body),
                             uncertain_decode=any(i.mnemonic in ('(bad)', '.byte') for i in body),
                             excerpt=[f'{i.address:08x}: {i.mnemonic} {i.operands}'.rstrip() for i in body[:12]]))
    unique = {}
    for row in rows:
        key = (row['bits'], row['start'], row['branch'])
        if key not in unique or (row['observed'] and not unique[key]['observed']):
            unique[key] = row
    return sorted(unique.values(), key=lambda row: (-row['taken'], row['category'], row['start']))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('snapshot', type=Path)
    parser.add_argument('--ram-base', type=lambda value: int(value, 0), default=0x100000)
    parser.add_argument('--le', type=Path, help='Original executable, for executable object boundaries')
    parser.add_argument('--bias', type=lambda value: int(value, 0), default=0)
    parser.add_argument('--region', action='append', default=[], help='NAME:LOW:HIGH:BITS (hex addresses allowed)')
    parser.add_argument('--trace', type=Path, action='append', default=[], help='WaitTrace JSON from a real run; repeat for several phases')
    parser.add_argument('--observed-segments', action='store_true', help='Also scan bounded code segments observed in the trace, clipped to this RAM snapshot')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    regions = executable_regions(args.le, args.bias) if args.le else []
    for raw in args.region:
        name, low, high, bits = raw.rsplit(':', 3)
        bits = int(bits)
        if bits not in (16, 32):
            parser.error('region bits must be 16 or 32')
        regions.append((name, int(low, 0), int(high, 0), bits))
    trace = merge_traces([json.loads(path.read_text()) for path in args.trace])
    if args.observed_segments:
        bounds = (args.ram_base, args.ram_base + args.snapshot.stat().st_size)
        spans = {}
        for segment in trace.get('segments', []):
            if segment['limit'] >= 0x1000000:
                continue  # Flat address space does not describe executable boundaries.
            low = max(bounds[0], segment.get('observed_low', segment['base']))
            high = min(bounds[1], segment.get('observed_high', segment['base'] + segment['limit'] + 1))
            if low < high:
                spans.setdefault(segment['bits'], []).append((low, high))
        for bits, intervals in spans.items():
            merged = []
            for low, high in sorted(intervals):
                if merged and low <= merged[-1][1]:
                    merged[-1] = (merged[-1][0], max(high, merged[-1][1]))
                else:
                    merged.append((low, high))
            for low, high in merged:
                regions.append((f'observed-code-{bits}', low, high, bits))
    if not regions:
        parser.error('provide --le, --region or --observed-segments with trace metadata; scanning arbitrary RAM as code is misleading')
    rows = scan(args.snapshot, args.ram_base, regions, trace)
    report = dict(regions=regions, trace_steps=trace.get('steps', 0), candidates=rows,
                  dropped=trace.get('dropped', 0),
                  limitation='Static candidates are not proof of a wait, reachability, or safe acceleration. Linear decoding can include inline data. Runtime evidence covers only the recorded scenario.',
                  counts=dict(Counter(row['category'] for row in rows)))
    args.output.write_text(json.dumps(report, indent=2) + '\n')
    print(json.dumps({key: report[key] for key in ('regions', 'trace_steps', 'counts')}))
    for row in rows[:25]:
        print(f'{row["start"]:08x}..{row["branch"]:08x} {row["category"]} instructions={row["instructions"]} taken={row["taken"]} ports={row["port_hints"]}')


def merge_traces(traces):
    result = dict(steps=0, dropped=0)
    for kind, keys, count in [('branches', ('source','target','bits'), 'taken'),
                              ('ports', ('source','port'), 'reads'),
                              ('services', ('vector','ax'), 'calls'),
                              ('halts', ('source',), 'calls'),
                              ('segments', ('base','limit','bits'), 'steps')]:
        entries = {}
        for trace in traces:
            for row in trace.get(kind, []):
                key = tuple(row[field] for field in keys)
                if key not in entries:
                    entries[key] = dict(row)
                    continue
                entry = entries[key]
                entry[count] += row[count]
                if kind == 'branches':
                    entry['fallthrough'] += row['fallthrough']
                if kind == 'ports':
                    entry['and'] &= row['and']
                    entry['or'] |= row['or']
                if kind == 'segments':
                    entry['observed_low'] = min(entry.get('observed_low', row['base']), row.get('observed_low', row['base']))
                    entry['observed_high'] = max(entry.get('observed_high', row['base'] + row['limit'] + 1), row.get('observed_high', row['base'] + row['limit'] + 1))
        result[kind] = list(entries.values())
    result['steps'] = sum(trace.get('steps', 0) for trace in traces)
    result['dropped'] = sum(trace.get('dropped', 0) for trace in traces)
    return result


if __name__ == '__main__':
    main()
