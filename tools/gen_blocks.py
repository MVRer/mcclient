#!/usr/bin/env python3
"""Generate src/world/blocks_<protocol>.rs: the block-state tables of each supported protocol.

Per block: its name (no namespace), its first state id and its properties (name + values). A
state's properties are decoded mixed-radix from `state - first`: properties sorted by name (vanilla's
StateDefinition keeps them in an ImmutableSortedMap; the report lists them in declaration order,
e.g. chest's `type, facing, waterlogged`), the LAST one varying fastest, each property's values in
the order the report lists them. The script asserts that this rule reproduces every state id of
every report, and that blocks tile the id space without gaps.

Sources:
  - vanilla data generator reports (`java -cp <client jar + libraries> net.minecraft.data.Main
    --reports`): generated/reports/blocks.json, for the versions given with --report;
  - PrismarineJS minecraft-data blocks.json (`minStateId`, `maxStateId`, `states` = property
    definitions) for a version without a local jar (--mcd). It has no per-state ids, so the script
    checks its id ranges and cross-checks its properties and default states against the reports of
    neighbouring versions (--check-against).

Usage (Python 3 standard library only):
  python3 tools/gen_blocks.py --out src/world \\
      --report 777=gen263/generated/reports/blocks.json \\
      --report 776=gen262/generated/reports/blocks.json \\
      --report 774=gen12111/generated/reports/blocks.json \\
      --mcd 775=mcd-26.1-blocks.json --check-against 775=776,774
"""

import argparse
import json
import math
import os
import sys

NAMES = {777: '26.3', 776: '26.2', 775: '26.1', 774: '1.21.11'}


def strip(name):
    return name[len('minecraft:'):] if name.startswith('minecraft:') else name


def mixed_radix(props, values):
    """Index of a state within its block: props = [(name, [values])], values = {name: value}."""
    index = 0
    for name, vals in props:
        index = index * len(vals) + vals.index(values[name])
    return index


def decode(props, index):
    out = {}
    for name, vals in reversed(props):
        out[name] = vals[index % len(vals)]
        index //= len(vals)
    return out


def load_report(path):
    """[(name, first, props, default_props)] sorted by first state id, every state id checked."""
    report = json.load(open(path))
    blocks = []
    for key, b in report.items():
        props = sorted((k, list(v)) for k, v in b.get('properties', {}).items())
        ids = [s['id'] for s in b['states']]
        first = min(ids)
        count = math.prod(len(v) for _, v in props)
        assert len(ids) == count, f'{key}: {len(ids)} states, properties give {count}'
        default = None
        for s in b['states']:
            got = first + mixed_radix(props, s.get('properties', {}))
            assert got == s['id'], f'{key}: state {s} decodes to {got}'
            if s.get('default'):
                default = s.get('properties', {})
        assert default is not None, f'{key}: no default state'
        blocks.append((strip(key), first, props, default))
    blocks.sort(key=lambda b: b[1])
    check_tiling(path, blocks)
    return blocks


def load_mcd(path):
    """minecraft-data blocks.json, in the same shape (default from `defaultState`)."""
    blocks = []
    for b in json.load(open(path)):
        props = []
        for p in b['states']:
            if p['type'] == 'bool':
                vals = ['true', 'false']  # vanilla BooleanProperty order
            else:
                vals = [str(v) for v in p['values']]
            assert len(vals) == p['num_values'], f'{b["name"]}: {p}'
            props.append((p['name'], vals))
        props.sort()
        count = math.prod(len(v) for _, v in props)
        first, last = b['minStateId'], b['maxStateId']
        assert last - first + 1 == count, f'{b["name"]}: ids {first}..{last}, properties give {count}'
        assert first <= b['defaultState'] <= last
        blocks.append((b['name'], first, props, decode(props, b['defaultState'] - first)))
    blocks.sort(key=lambda b: b[1])
    check_tiling(path, blocks)
    return blocks


def check_tiling(path, blocks):
    expect = 0
    for name, first, props, _ in blocks:
        assert first == expect, f'{path}: {name} starts at {first}, expected {expect}'
        expect += math.prod(len(v) for _, v in props)
    assert blocks[0][0] == 'air' and blocks[0][1] == 0


def total_states(blocks):
    name, first, props, _ = blocks[-1]
    return first + math.prod(len(v) for _, v in props)


def cross_check(protocol, blocks, others):
    """For an mcd-sourced version: blocks also in a neighbouring report should list the same
    properties in the same order with the same values, and have the same default state (decoded
    with the mixed-radix rule from minecraft-data's id). Against the first (closest) version listed
    this must hold for at least 99% of the blocks and every default must agree; later versions are
    reported only (vanilla does change things, e.g. 26.1 added note block instruments and moved the
    standing sign's default rotation from 0 to 8)."""
    for n, (other_protocol, other) in enumerate(others):
        by_name = {b[0]: b for b in other}
        same = differ = defaults_differ = 0
        for name, _, props, default in blocks:
            o = by_name.get(name)
            if o is None:
                continue
            if o[2] == props:
                same += 1
                if o[3] != default:
                    defaults_differ += 1
                    assert n > 0, f'{protocol} vs {other_protocol}: {name} default {default} != {o[3]}'
            else:
                differ += 1
                print(f'  note: {name} properties differ between {protocol} and {other_protocol}',
                      file=sys.stderr)
        print(f'  {protocol} vs {other_protocol}: {same} blocks with identical properties'
              f' ({defaults_differ} of them with another default state), {differ} with different'
              f' properties, {len(blocks) - same - differ} not in {other_protocol}', file=sys.stderr)
        if n == 0:
            assert same >= 0.99 * len(blocks)


def rust_table(protocol, blocks, source):
    props_index = {}
    props_list = []
    rows = []
    for name, first, props, _ in blocks:
        refs = []
        for pname, vals in props:
            key = (pname, tuple(vals))
            if key not in props_index:
                props_index[key] = len(props_list)
                props_list.append(key)
            refs.append(props_index[key])
        assert len(props_list) <= 256
        rows.append(f'({json.dumps(name)}, {first}, &[{", ".join(map(str, refs))}])')
    states = total_states(blocks)
    out = [
        f'//! Block states of protocol {protocol} ({NAMES[protocol]}): {len(blocks)} blocks, {states} states.',
        '//! Generated by `tools/gen_blocks.py` (do not edit by hand) from',
        *[f'//! {line}' for line in source],
        '',
        'use super::blocks::Table;',
        '',
        '#[rustfmt::skip]',
        'pub(super) static TABLE: Table = Table {',
        f'    states: {states},',
        '    props: &[',
    ]
    for pname, vals in props_list:
        out.append(f'        ({json.dumps(pname)}, &[{", ".join(json.dumps(v) for v in vals)}]),')
    out.append('    ],')
    out.append('    blocks: &[')
    line = ''
    for row in rows:
        if line and len(line) + len(row) + 2 > 100:
            out.append(line.rstrip())
            line = ''
        line = (line or '        ') + row + ', '
    if line:
        out.append(line.rstrip())
    out.append('    ],')
    out.append('};')
    return '\n'.join(out) + '\n'


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out', required=True, help='directory for blocks_<protocol>.rs')
    ap.add_argument('--report', action='append', default=[], metavar='PROTOCOL=PATH')
    ap.add_argument('--mcd', action='append', default=[], metavar='PROTOCOL=PATH')
    ap.add_argument('--check-against', action='append', default=[], metavar='PROTOCOL=P1,P2')
    args = ap.parse_args()

    tables = {}
    sources = {}
    for spec in args.report:
        p, path = spec.split('=', 1)
        tables[int(p)] = load_report(path)
        sources[int(p)] = ['the vanilla data generator report `reports/blocks.json`.']
        print(f'{p}: {len(tables[int(p)])} blocks, {total_states(tables[int(p)])} states,'
              ' every state id reproduced by the mixed-radix rule', file=sys.stderr)
    for spec in args.mcd:
        p, path = spec.split('=', 1)
        tables[int(p)] = load_mcd(path)
        sources[int(p)] = ['PrismarineJS minecraft-data `blocks.json` (no vanilla report for this version: id',
                           'ranges checked, properties and default states cross-checked against the reports',
                           'of neighbouring versions).']
        print(f'{p}: {len(tables[int(p)])} blocks, {total_states(tables[int(p)])} states (minecraft-data)',
              file=sys.stderr)
    for spec in args.check_against:
        p, others = spec.split('=', 1)
        cross_check(int(p), tables[int(p)], [(int(o), tables[int(o)]) for o in others.split(',')])

    for protocol, blocks in sorted(tables.items(), reverse=True):
        path = os.path.join(args.out, f'blocks_{protocol}.rs')
        with open(path, 'w') as f:
            f.write(rust_table(protocol, blocks, sources[protocol]))
        print(f'wrote {path} ({os.path.getsize(path)} bytes, global palette'
              f' {math.ceil(math.log2(total_states(blocks)))} bits)', file=sys.stderr)


if __name__ == '__main__':
    main()
