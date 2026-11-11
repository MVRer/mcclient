#!/usr/bin/env python3
"""Generate src/ids.rs: the packet ids of every packet the client sends or reads, per protocol.

Sources:
  - vanilla data generator reports (generated/reports/packets.json) for the versions given with
    --report (777 = 26.3, 776 = 26.2, 774 = 1.21.11);
  - for 775 (26.1), which has no report here: Horizon's generated version adapter
    (crates/horizon-protocol/src/adapter/v26_1_2.rs), which maps 26.1 ids to 26.3 ids. The script
    first checks that the adapters of the reported versions (v26_2.rs, v1_21_11.rs) reproduce the
    reports exactly; 26.1's `swing` (which has no 26.3 counterpart) comes from minecraft-data.

Usage (Python 3 standard library only):
  python3 tools/gen_ids.py --out src/ids.rs \\
      --report 777=gen263/generated/reports/packets.json \\
      --report 776=gen262/generated/reports/packets.json \\
      --report 774=gen12111/generated/reports/packets.json \\
      --horizon path/to/horizon --mcd-protocol-775 minecraft-data/data/pc/26.1/protocol.json
"""

import argparse
import json
import re
import sys

CFG = {
    'clientbound': ['cookie_request', 'disconnect', 'finish_configuration', 'keep_alive', 'ping', 'registry_data',
                    'resource_pack_push', 'select_known_packs', 'code_of_conduct'],
    'serverbound': ['client_information', 'cookie_response', 'custom_payload', 'finish_configuration', 'keep_alive',
                    'pong', 'resource_pack', 'select_known_packs', 'accept_code_of_conduct'],
}
PLAY = {
    'clientbound': ['add_entity', 'block_update', 'chunk_batch_finished', 'chunk_batch_start', 'container_close',
                    'container_set_content', 'container_set_slot', 'cookie_request', 'disconnect', 'disguised_chat',
                    'entity_position_sync', 'forget_level_chunk', 'game_event', 'keep_alive', 'level_chunk_with_light',
                    'login', 'move_entity_pos', 'move_entity_pos_rot', 'move_entity_rot', 'open_screen',
                    'open_sign_editor', 'ping', 'player_abilities', 'player_chat', 'player_combat_kill',
                    'player_info_remove', 'player_info_update', 'player_position', 'remove_entities',
                    'resource_pack_push', 'respawn', 'section_blocks_update', 'set_action_bar_text', 'set_cursor_item',
                    'set_entity_data', 'set_experience', 'set_health', 'set_held_slot', 'set_player_inventory',
                    'set_subtitle_text', 'set_title_text', 'start_configuration', 'system_chat', 'teleport_entity'],
    'serverbound': ['accept_teleportation', 'chat', 'chat_command', 'chunk_batch_received', 'client_command',
                    'client_tick_end', 'configuration_acknowledged', 'container_click', 'container_close',
                    'cookie_response', 'interact', 'keep_alive', 'move_player_pos', 'move_player_pos_rot',
                    'move_player_rot', 'move_player_status_only', 'player_action', 'player_command', 'player_input',
                    'player_loaded', 'pong', 'resource_pack', 'set_carried_item', 'set_creative_mode_slot',
                    'sign_update', 'swing', 'punch', 'attack', 'use_item', 'use_item_on'],
}
# Packets some versions don't have: written as -1.
OPTIONAL = {('play', 'serverbound', n) for n in ('attack', 'swing', 'punch')}
NAMES = {777: '26.3', 776: '26.2', 775: '26.1', 774: '1.21.11'}
ADAPTERS = {776: 'v26_2.rs', 775: 'v26_1_2.rs', 774: 'v1_21_11.rs'}

HEADER = '''//! Packet ids per protocol version for every packet this client sends or reads.
//!
//! Generated from the vanilla data generator's `packets.json` for 777 (26.3), 776 (26.2) and 774
//! (1.21.11); 775 (26.1) via Horizon's generated version adapter (`adapter/v26_1_2.rs`), which
//! reproduces the vanilla ids exactly for 776 and 774. Do not edit by hand.
//!
//! Handshake, status and login ids are the same in every supported version.

pub mod handshake {
    pub const INTENTION: i32 = 0x00;
    pub const INTENT_STATUS: i32 = 1;
    pub const INTENT_LOGIN: i32 = 2;
}

pub mod status {
    pub const SB_STATUS_REQUEST: i32 = 0x00;
    pub const CB_STATUS_RESPONSE: i32 = 0x00;
}

pub mod login {
    pub const CB_DISCONNECT: i32 = 0x00;
    pub const CB_ENCRYPTION_REQUEST: i32 = 0x01;
    pub const CB_LOGIN_FINISHED: i32 = 0x02;
    pub const CB_SET_COMPRESSION: i32 = 0x03;
    pub const CB_CUSTOM_QUERY: i32 = 0x04;
    pub const CB_COOKIE_REQUEST: i32 = 0x05;

    pub const SB_HELLO: i32 = 0x00;
    pub const SB_CUSTOM_QUERY_ANSWER: i32 = 0x02;
    pub const SB_LOGIN_ACKNOWLEDGED: i32 = 0x03;
    pub const SB_COOKIE_RESPONSE: i32 = 0x04;
}
'''


def adapter(path):
    """Horizon's per-state id tables: for clientbound, old id -> 26.3 id; for serverbound, 26.3 id -> old id."""
    src = open(path).read()
    out = {}
    for st in ['CONFIGURATION_CLIENTBOUND', 'CONFIGURATION_SERVERBOUND', 'PLAY_CLIENTBOUND', 'PLAY_SERVERBOUND']:
        body = re.search(r'const ' + st + r': &\[Option<u8>\] = &\[(.*?)\];', src, re.S).group(1)
        out[st] = [None if t == 'None' else int(t[5:-1]) for t in re.findall(r'Some\(\d+\)|None', body)]
    return out


def via_adapter(tab, state, direction, pinned):
    key = ('CONFIGURATION' if state == 'configuration' else 'PLAY') + '_' + direction.upper()
    if direction == 'clientbound':
        hits = [i for i, v in enumerate(tab[key]) if v == pinned]
        return hits[0] if hits else None
    return tab[key][pinned] if pinned < len(tab[key]) else None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('--out', required=True)
    ap.add_argument('--report', action='append', required=True, metavar='PROTOCOL=PATH')
    ap.add_argument('--horizon', required=True, help='a checkout of github.com/horizon-rust/horizon')
    ap.add_argument('--mcd-protocol-775', required=True, help="minecraft-data's data/pc/26.1/protocol.json")
    a = ap.parse_args()

    reports = {int(p): json.load(open(path)) for p, path in (r.split('=', 1) for r in a.report)}
    adapters = {p: adapter(f'{a.horizon}/crates/horizon-protocol/src/adapter/{f}') for p, f in ADAPTERS.items()}
    mcd = json.load(open(a.mcd_protocol_775))

    def official(proto, state, direction, name):
        return reports[proto][state][direction].get('minecraft:' + name, {}).get('protocol_id')

    def mcd775(state, direction, name):
        types = mcd[state]['toClient' if direction == 'clientbound' else 'toServer']['types']
        mapping = types['packet'][1][0]['type'][1]['mappings']
        return {v: int(k, 16) for k, v in mapping.items()}.get(name)

    ids = {p: {} for p in (777, 776, 775, 774)}
    problems = []
    for state, groups in (('configuration', CFG), ('play', PLAY)):
        for direction, names in groups.items():
            for n in names:
                key = (state, direction, n)
                if key == ('play', 'serverbound', 'swing'):
                    # 26.3 replaced swing with punch, so there is nothing to map 26.1's through.
                    for p in (777, 776, 774):
                        v = official(p, *key)
                        ids[p][key] = -1 if v is None else v
                    ids[775][key] = mcd775(state, direction, 'arm_animation')
                    continue
                pinned = official(777, *key)
                if pinned is None:
                    problems.append(f'777 has no {key}')
                    continue
                for p in (777, 776, 775, 774):
                    if p in reports:
                        v = official(p, *key)
                        if p != 777:
                            mapped = via_adapter(adapters[p], state, direction, pinned)
                            if mapped != v and not (mapped is None and v is None):
                                problems.append(f'adapter {p} {key}: horizon {mapped} vs report {v}')
                    else:
                        v = via_adapter(adapters[p], state, direction, pinned)
                    if v is None and key in OPTIONAL:
                        v = -1
                    if v is None:
                        problems.append(f'{p} lacks {key}')
                    ids[p][key] = v
    if problems:
        sys.exit('problems:\n  ' + '\n  '.join(problems))

    def fields(groups):
        return [(d, n) for d in ('clientbound', 'serverbound') for n in groups[d]]

    def pre(d):
        return 'cb_' if d == 'clientbound' else 'sb_'

    out = [HEADER]
    for sname, groups, doc in (('Config', CFG, 'Configuration-state'), ('Play', PLAY, 'Play-state')):
        out += [f'/// {doc} ids (`cb_` = clientbound, `sb_` = serverbound).', '#[derive(Debug)]', f'pub struct {sname} {{']
        out += [f'    pub {pre(d)}{n}: i32,' for d, n in fields(groups)]
        out += ['}', '']
    out.append('/// Everything that differs between protocol versions. A play id of -1: the version has no such packet.'
               '\n#[derive(Debug)]\npub struct Ids {\n    pub protocol: i32,\n    pub name: &\'static str,\n'
               '    pub cfg: Config,\n    pub play: Play,\n}\n')

    def value(v):
        return f'0x{v:02X}' if v >= 0 else '-1'

    for p in (777, 776, 775, 774):
        out.append(f'/// {NAMES[p]}.')
        out.append(f'pub const V{p}: Ids = Ids {{\n    protocol: {p},\n    name: "{NAMES[p]}",\n    cfg: Config {{')
        out += [f'        {pre(d)}{n}: {value(ids[p][("configuration", d, n)])},' for d, n in fields(CFG)]
        out.append('    },\n    play: Play {')
        out += [f'        {pre(d)}{n}: {value(ids[p][("play", d, n)])},' for d, n in fields(PLAY)]
        out.append('    },\n};\n')
    out.append('/// Newest first; the first entry is the default.')
    out.append('pub static SUPPORTED: [&Ids; 4] = [&V777, &V776, &V775, &V774];\n')
    out.append("pub fn for_protocol(protocol: i32) -> Option<&'static Ids> {\n"
               '    SUPPORTED.iter().copied().find(|ids| ids.protocol == protocol)\n}\n')
    out.append('/// "777 (26.3), 776 (26.2), ..." for messages.\npub fn supported_list() -> String {\n    SUPPORTED\n'
               '        .iter()\n        .map(|ids| format!("{} ({})", ids.protocol, ids.name))\n'
               '        .collect::<Vec<_>>()\n        .join(", ")\n}')
    open(a.out, 'w').write('\n'.join(out) + '\n')


if __name__ == '__main__':
    main()
