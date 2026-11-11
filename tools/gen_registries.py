#!/usr/bin/env python3
"""Generate src/registries.rs: the built-in registries the protocol refers to by numeric id (items,
entity types, menus, data component types), per protocol, in protocol-id order.

Sources:
  - vanilla data generator reports (generated/reports/registries.json) for the versions given with
    --report (777 = 26.3, 776 = 26.2, 774 = 1.21.11);
  - PrismarineJS minecraft-data for 775 (26.1): items.json, entities.json and protocol.json (whose
    SlotComponentType mapper is the data component order). Menus are identical in every version,
    so 775 uses 777's.

Usage (Python 3 standard library only):
  python3 tools/gen_registries.py --out src/registries.rs \\
      --report 777=gen263/generated/reports/registries.json \\
      --report 776=gen262/generated/reports/registries.json \\
      --report 774=gen12111/generated/reports/registries.json \\
      --mcd-775 minecraft-data/data/pc/26.1
"""

import argparse
import json

REGISTRIES = ['item', 'entity_type', 'menu', 'data_component_type']

HEADER = """//! Built-in (non-synchronized) registries the protocol refers to by numeric id, per protocol
//! version, in protocol-id order, without the `minecraft:` namespace.
//!
//! Generated from the vanilla data generator reports (`registries.json`) of 26.3 (777), 26.2 (776)
//! and 1.21.11 (774); 26.1 (775) from PrismarineJS minecraft-data. Do not edit by hand.
//!
//! Data-driven registries (enchantments, chat types, dimension types, ...) are not here: the
//! server sends them during configuration and the client records them ([`Dynamic`]).

use std::collections::HashMap;

/// The built-in registries of one protocol version.
pub struct Registries {
    pub item: &'static [&'static str],
    pub entity_type: &'static [&'static str],
    pub menu: &'static [&'static str],
    pub data_component_type: &'static [&'static str],
}

/// Entry names of the registries the server sent in configuration (Registry Data), keyed by
/// registry name without namespace (e.g. `"enchantment"`, `"chat_type"`), in id order.
pub type Dynamic = HashMap<String, Vec<String>>;

impl Registries {
    /// A name for an id, or `#<id>` when out of range.
    pub fn name(table: &[&str], id: i32) -> String {
        usize::try_from(id).ok().and_then(|i| table.get(i)).map_or_else(|| format!("#{id}"), |s| s.to_string())
    }
}

pub fn for_protocol(protocol: i32) -> &'static Registries {
    match protocol {
        776 => &R776,
        775 => &R775,
        774 => &R774,
        _ => &R777,
    }
}
"""


def strip(name):
    return name[len('minecraft:'):] if name.startswith('minecraft:') else name


def find_mapper(node, key):
    if isinstance(node, dict):
        if key in node:
            return node[key]
        children = node.values()
    elif isinstance(node, list):
        children = node
    else:
        return None
    for child in children:
        found = find_mapper(child, key)
        if found is not None:
            return found
    return None


def array(name, values):
    lines = ['#[rustfmt::skip]', f'static {name}: &[&str] = &[']
    row = []
    for v in values:
        row.append(json.dumps(v))
        if len(', '.join(row)) > 90:
            lines.append('    ' + ', '.join(row) + ',')
            row = []
    if row:
        lines.append('    ' + ', '.join(row) + ',')
    lines.append('];')
    return '\n'.join(lines)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out', required=True)
    ap.add_argument('--report', action='append', required=True, metavar='PROTOCOL=PATH')
    ap.add_argument('--mcd-775', required=True, help="minecraft-data's data/pc/26.1 directory")
    a = ap.parse_args()

    tables = {}
    for proto, path in (r.split('=', 1) for r in a.report):
        report = json.load(open(path))
        tables[int(proto)] = {
            r: [strip(k) for k, _ in sorted(report['minecraft:' + r]['entries'].items(), key=lambda kv: kv[1]['protocol_id'])]
            for r in REGISTRIES
        }
    mcd = a.mcd_775.rstrip('/')
    items = [x['name'] for x in sorted(json.load(open(f'{mcd}/items.json')), key=lambda x: x['id'])]
    entities = [x['name'] for x in sorted(json.load(open(f'{mcd}/entities.json')), key=lambda x: x['id'])]
    mappings = find_mapper(json.load(open(f'{mcd}/protocol.json')), 'SlotComponentType')[1]['mappings']
    components = [strip(mappings[k]) for k in sorted(mappings, key=lambda k: int(k, 16) if k.startswith('0x') else int(k))]
    tables[775] = {'item': items, 'entity_type': entities, 'menu': tables[777]['menu'], 'data_component_type': components}

    out = [HEADER]
    for p in (777, 776, 775, 774):
        fields = ', '.join(f'{r}: {r.upper()}_{p}' for r in REGISTRIES)
        out.append(f'static R{p}: Registries = Registries {{ {fields} }};')
    out.append('')
    for p in (777, 776, 775, 774):
        for r in REGISTRIES:
            out.append(array(f'{r.upper()}_{p}', tables[p][r]))
            out.append('')
    open(a.out, 'w').write('\n'.join(out))


if __name__ == '__main__':
    main()
