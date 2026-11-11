use super::*;
use crate::net::write_varint;
use crate::registries::for_protocol;

/// A byte fixture under construction, with the ids of one protocol's registries.
struct B {
    p: i32,
    data: Vec<u8>,
}

impl B {
    fn new(p: i32) -> Self {
        Self { p, data: Vec::new() }
    }
    fn v(mut self, x: i32) -> Self {
        write_varint(&mut self.data, x);
        self
    }
    fn raw(mut self, bytes: &[u8]) -> Self {
        self.data.extend_from_slice(bytes);
        self
    }
    fn bool(self, b: bool) -> Self {
        self.raw(&[b as u8])
    }
    fn s(self, s: &str) -> Self {
        self.v(s.len() as i32).raw(s.as_bytes())
    }
    fn f(self, x: f32) -> Self {
        self.raw(&x.to_be_bytes())
    }
    fn d(self, x: f64) -> Self {
        self.raw(&x.to_be_bytes())
    }
    fn i(self, x: i32) -> Self {
        self.raw(&x.to_be_bytes())
    }
    fn l(self, x: i64) -> Self {
        self.raw(&x.to_be_bytes())
    }
    /// A text component as network NBT: a bare string tag.
    fn text(self, s: &str) -> Self {
        self.raw(&[8]).raw(&(s.len() as u16).to_be_bytes()).raw(s.as_bytes())
    }
    /// A component type id by name.
    fn c(self, name: &str) -> Self {
        let id = component_id(self.p, name);
        self.v(id)
    }
    /// An item id by name.
    fn item(self, name: &str) -> Self {
        let id = item_id(self.p, name);
        self.v(id)
    }
}

fn component_id(p: i32, name: &str) -> i32 {
    for_protocol(p).data_component_type.iter().position(|n| *n == name).unwrap_or_else(|| panic!("{p} has no {name}"))
        as i32
}
fn item_id(p: i32, name: &str) -> i32 {
    for_protocol(p).item.iter().position(|n| *n == name).unwrap_or_else(|| panic!("{p} has no {name}")) as i32
}

/// Network NBT for `{a: 1, b: "x", c: [I; 1, 2] as a list of ints}`.
fn compound() -> Vec<u8> {
    let mut b = vec![10];
    b.extend([3, 0, 1, b'a', 0, 0, 0, 1]);
    b.extend([8, 0, 1, b'b', 0, 1, b'x']);
    b.extend([9, 0, 1, b'c', 3, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2]);
    b.push(0);
    b
}

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

struct Fixture {
    lang: Lang,
    dynamic: Dynamic,
}

impl Fixture {
    fn new() -> Self {
        let mut dynamic = Dynamic::new();
        dynamic.insert(
            "enchantment".into(),
            ["minecraft:aqua_affinity", "minecraft:sharpness", "minecraft:unbreaking", "mymod:zap"]
                .map(String::from)
                .to_vec(),
        );
        Self { lang: Lang::load(None).unwrap(), dynamic }
    }
    fn cx(&self, p: i32) -> ItemContext<'_> {
        ItemContext { registries: for_protocol(p), dynamic: &self.dynamic, lang: &self.lang }
    }
    /// Reads one slot and requires every byte to be consumed.
    fn one(&self, p: i32, bytes: &[u8]) -> Option<ItemStack> {
        let mut r = Reader::new(bytes);
        let stack = read_slot(&mut r, &self.cx(p)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(r.remaining(), 0, "{stack:?} left {} bytes", r.remaining());
        stack
    }
    fn err(&self, p: i32, bytes: &[u8]) -> String {
        match read_slot(&mut Reader::new(bytes), &self.cx(p)) {
            Ok(s) => panic!("expected an error, got {s:?}"),
            Err(e) => e.to_string(),
        }
    }
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn empty_slots() {
    let f = Fixture::new();
    assert_eq!(f.one(777, &[0]), None);
    // A negative count is empty too, and nothing follows it.
    assert_eq!(f.one(777, &B::new(777).v(-5).data), None);
}

#[test]
fn a_plain_item() {
    let f = Fixture::new();
    let stack = f.one(777, &B::new(777).v(64).item("diamond").v(0).v(0).data).unwrap();
    assert_eq!(stack, ItemStack { item: "diamond".into(), count: 64, ..ItemStack::default() });
    assert_eq!(stack.display_name(), None);
    assert_eq!(stack.to_json(), serde_json::json!({"item": "diamond", "count": 64}));
    // An id past the item table.
    let stack = f.one(777, &B::new(777).v(1).v(99_999).v(0).v(0).data).unwrap();
    assert_eq!(stack.item, "#99999");
}

#[test]
fn names_lore_enchantments_and_durability() {
    let f = Fixture::new();
    let translated = {
        // {translate: "multiplayer.player.joined", with: ["Alex"]} as NBT.
        let mut b = vec![10, 8, 0, 9];
        b.extend(b"translate");
        b.extend([0, 25]);
        b.extend(b"multiplayer.player.joined");
        b.extend([9, 0, 4]);
        b.extend(b"with");
        b.extend([8, 0, 0, 0, 1, 0, 4]);
        b.extend(b"Alex");
        b.push(0);
        b
    };
    let bytes = B::new(777)
        .v(1)
        .item("diamond_sword")
        .v(7)
        .v(0)
        .c("custom_name")
        .text("§6Excalibur")
        .c("lore")
        .v(2)
        .text("Forged in")
        .raw(&translated)
        .c("enchantments")
        .v(3)
        .v(1)
        .v(5)
        .v(3)
        .v(1)
        .v(9)
        .v(2)
        .c("damage")
        .v(17)
        .c("max_damage")
        .v(1561)
        .c("unbreakable")
        .c("item_name")
        .text("Sword")
        .data;
    let stack = f.one(777, &bytes).unwrap();
    assert_eq!(stack.custom_name.as_deref(), Some("Excalibur"));
    assert_eq!(stack.item_name.as_deref(), Some("Sword"));
    assert_eq!(stack.display_name(), Some("Excalibur"));
    assert_eq!(stack.lore, names(&["Forged in", "Alex joined the game"]));
    assert_eq!(
        stack.enchantments,
        [("sharpness".to_string(), 5), ("mymod:zap".to_string(), 1), ("#9".to_string(), 2)]
    );
    assert_eq!((stack.damage, stack.max_damage, stack.unbreakable), (Some(17), Some(1561), true));
    assert_eq!(
        stack.components,
        names(&["custom_name", "lore", "enchantments", "damage", "max_damage", "unbreakable", "item_name"])
    );
    assert_eq!(
        stack.to_json(),
        serde_json::json!({
            "item": "diamond_sword", "count": 1, "custom_name": "Excalibur", "item_name": "Sword",
            "lore": ["Forged in", "Alex joined the game"],
            "enchantments": {"sharpness": 5, "mymod:zap": 1, "#9": 2},
            "damage": 17, "max_damage": 1561, "unbreakable": true,
            "components": ["custom_name", "lore", "enchantments", "damage", "max_damage", "unbreakable", "item_name"],
        })
    );
    // Without the server's enchantment registry the ids stand in.
    let empty = Dynamic::new();
    let cx = ItemContext { registries: for_protocol(777), dynamic: &empty, lang: &f.lang };
    let stack = read_slot(&mut Reader::new(&bytes), &cx).unwrap().unwrap();
    assert_eq!(stack.enchantments[0], ("#1".to_string(), 5));
}

#[test]
fn player_heads_carry_a_profile_name() {
    let f = Fixture::new();
    // Either.left: a complete GameProfile (UUID, name, properties), then a skin patch with a body
    // texture and the slim model.
    let full = B::new(777)
        .v(1)
        .item("player_head")
        .v(1)
        .v(0)
        .c("profile")
        .bool(true)
        .raw(&0x7a5d_6a1a_30d7_4187_88c5_efe9_1201_8922u128.to_be_bytes())
        .s("Notch")
        .v(1)
        .s("textures")
        .s("dGV4dHVyZXM=")
        .bool(true)
        .s("c2ln")
        .bool(true)
        .s("minecraft:entity/player/wide/steve")
        .bool(false)
        .bool(false)
        .bool(true)
        .bool(true)
        .data;
    let stack = f.one(777, &full).unwrap();
    assert_eq!(stack.profile_name.as_deref(), Some("Notch"));
    assert_eq!(stack.to_json()["profile_name"], "Notch");
    // Either.right: a partial profile with only a name.
    let partial = B::new(777)
        .v(1)
        .item("player_head")
        .v(1)
        .v(0)
        .c("profile")
        .bool(false)
        .bool(true)
        .s("jeb_")
        .bool(false)
        .v(0)
        .raw(&[0, 0, 0, 0])
        .data;
    assert_eq!(f.one(777, &partial).unwrap().profile_name.as_deref(), Some("jeb_"));
    // Horizon's capture: a partial profile with an empty name, a UUID and a texture.
    let mut capture = vec![1, 1, 1, 0, 72, 0, 1, 0, 1];
    capture.extend(0x7a5d_6a1a_30d7_4187_88c5_efe9_1201_8922u128.to_be_bytes());
    capture.extend([1, 8]);
    capture.extend(b"textures");
    capture.extend([1, b'x', 0, 0, 0, 0, 0]);
    let stack = f.one(777, &capture).unwrap();
    assert_eq!((stack.profile_name, stack.components), (None, names(&["profile"])));
}

#[test]
fn removed_components_are_named() {
    let f = Fixture::new();
    let bytes = B::new(777)
        .v(1)
        .item("golden_apple")
        .v(1)
        .v(2)
        .c("rarity")
        .v(2)
        .c("consumable")
        .c("food")
        .data;
    let stack = f.one(777, &bytes).unwrap();
    assert_eq!(stack.components, names(&["rarity"]));
    assert_eq!(stack.removed, names(&["consumable", "food"]));
    assert_eq!(stack.to_json()["removed"], serde_json::json!(["consumable", "food"]));
}

/// The bytes of a mob effect: id, amplifier, duration, ambient, particles, icon, and a hidden
/// effect one level deep.
fn mob_effect(b: B) -> B {
    b.v(1).v(1).v(600).bool(false).bool(true).bool(true).bool(true).v(0).v(100).bool(false).bool(false).bool(false).bool(false)
}

/// A sound by direct value: 0, its id, a fixed range.
fn direct_sound(b: B) -> B {
    b.v(0).s("minecraft:x").bool(true).f(8.0)
}

/// Writes one component's value.
type Value = Box<dyn Fn(B) -> B>;

/// One stack with (nearly) every kind of value the decoder only reads past, in 26.3's layout, and
/// the components it adds.
fn everything(p: i32) -> (Vec<u8>, Vec<String>) {
    let mut list: Vec<(&str, Value)> = vec![
        ("custom_data", Box::new(|b| b.raw(&compound()))),
        ("max_stack_size", Box::new(|b| b.v(16))),
        ("use_effects", Box::new(|b| b.bool(true).bool(false).f(0.5))),
        ("minimum_attack_charge", Box::new(|b| b.f(0.9))),
        ("damage_type", Box::new(|b| b.v(3))),
        ("item_model", Box::new(|b| b.s("minecraft:stick"))),
        ("rarity", Box::new(|b| b.v(3))),
        // A block predicate with every part: a tag, two property matchers (exact and ranged),
        // NBT, an exact damage component and a partial predicate.
        ("can_break", Box::new(|b| {
            b.v(1)
                .bool(true).v(0).s("minecraft:logs")
                .bool(true).v(2).s("snowy").bool(true).s("true").s("age").bool(false).bool(true).s("1").bool(false)
                .bool(true).raw(&[10, 0])
                .v(1).c("damage").v(3)
                .v(1).bool(true).v(2).raw(&compound())
        })),
        ("can_place_on", Box::new(|b| b.v(1).bool(true).v(3).v(1).v(2).bool(false).bool(false).v(0).v(0))),
        ("attribute_modifiers", Box::new(|b| {
            b.v(2).v(1).s("minecraft:x").d(1.5).v(0).v(1).v(2).text("override").v(3).s("minecraft:y").d(-0.25).v(2).v(0).v(1)
        })),
        ("custom_model_data", Box::new(|b| b.v(2).f(1.0).f(2.5).v(2).bool(true).bool(false).v(1).s("a").v(1).i(0xff0000))),
        ("tooltip_display", Box::new(|b| b.bool(false).v(2).v(13).v(3))),
        ("repair_cost", Box::new(|b| b.v(7))),
        ("creative_slot_lock", Box::new(|b| b)),
        ("enchantment_glint_override", Box::new(|b| b.bool(true))),
        ("intangible_projectile", Box::new(|b| b.raw(&[10, 0]))),
        ("food", Box::new(|b| b.v(4).f(2.4).bool(false))),
        // Every consume effect type: apply_effects, remove_effects, clear_all_effects,
        // teleport_randomly, play_sound.
        ("consumable", Box::new(|b| {
            let b = b.f(1.6).v(1).v(5).bool(true).v(5);
            let b = mob_effect(b.v(0).v(1)).f(1.0);
            let b = b.v(1).v(0).s("minecraft:harmful").v(2).v(3).f(16.0).bool(true).v(4);
            direct_sound(b)
        })),
        ("use_remainder", Box::new(|b| b.item("bowl").v(1).v(1).v(0).c("damage").v(1))),
        ("use_cooldown", Box::new(|b| b.f(1.5).bool(true).s("minecraft:ender_pearl"))),
        ("damage_resistant", Box::new(|b| b.v(0).s("minecraft:is_fire"))),
        ("tool", Box::new(|b| b.v(1).v(3).v(4).v(5).bool(true).f(8.0).bool(true).bool(true).f(1.0).v(1).bool(false))),
        ("weapon", Box::new(|b| b.v(1).f(5.0))),
        ("attack_range", Box::new(|b| b.f(0.0).f(3.0).f(0.0).f(4.0).f(0.1).f(1.0))),
        ("enchantable", Box::new(|b| b.v(10))),
        ("equippable", Box::new(|b| {
            direct_sound(b.v(4)).bool(true).s("minecraft:diamond").bool(false).bool(true).v(2).v(7)
                .raw(&[1, 1, 1, 0, 0]).v(0x5a8)
        })),
        ("repairable", Box::new(|b| b.v(0).s("minecraft:planks"))),
        ("glider", Box::new(|b| b)),
        ("tooltip_style", Box::new(|b| b.s("minecraft:fancy"))),
        ("death_protection", Box::new(|b| b.v(2).v(2).v(1).v(3).v(10).v(11))),
        ("blocks_attacks", Box::new(|b| {
            b.f(0.25).f(1.0).v(1).f(90.0).bool(true).v(0).s("minecraft:arrow").f(0.0).f(1.0)
                .f(3.0).f(1.0).f(1.0).bool(true).v(0).s("minecraft:bypasses_shield").bool(true).v(9).bool(false)
        })),
        ("piercing_weapon", Box::new(|b| direct_sound(b.bool(true).bool(false).bool(true)).bool(true).v(12))),
        ("kinetic_weapon", Box::new(|b| {
            b.v(10).v(0).bool(true).v(100).f(0.5).f(0.1).bool(false).bool(false).f(1.0).f(1.5).bool(true).v(4).bool(false)
        })),
        ("attack_animation", Box::new(|b| b.v(1).v(9))),
        ("interact_animation", Box::new(|b| b.v(2).v(6))),
        ("additional_trade_cost", Box::new(|b| b.v(2))),
        ("block_transformer", Box::new(|b| b.v(1))),
        ("villager_food", Box::new(|b| b.v(1))),
        ("stored_enchantments", Box::new(|b| b.v(1).v(2).v(3))),
        ("dye", Box::new(|b| b.v(14))),
        ("dyed_color", Box::new(|b| b.i(-6265536))),
        ("map_id", Box::new(|b| b.v(42))),
        ("map_decorations", Box::new(|b| b.raw(&compound()))),
        ("map_post_processing", Box::new(|b| b.v(1))),
        ("charged_projectiles", Box::new(|b| {
            b.v(2).item("arrow").v(1).v(0).v(0).item("firework_rocket").v(1).v(1).v(0).c("fireworks").v(2).v(1).v(1).v(1).i(5).v(0).bool(false).bool(false)
        })),
        // A bundle holding a bundle holding stone.
        ("bundle_contents", Box::new(|b| {
            b.v(1).item("bundle").v(1).v(1).v(0).c("bundle_contents").v(1).item("stone").v(32).v(0).v(0)
        })),
        ("potion_contents", Box::new(|b| {
            mob_effect(b.bool(true).v(31).bool(true).i(0x00ff_8800).v(1)).bool(true).s("Tea")
        })),
        ("potion_duration_scale", Box::new(|b| b.f(0.25))),
        ("suspicious_stew_effects", Box::new(|b| b.v(2).v(15).v(100).v(18).v(160))),
        ("writable_book_content", Box::new(|b| b.v(2).s("a").bool(false).s("b").bool(true).s("c"))),
        ("written_book_content", Box::new(|b| {
            b.s("T").bool(true).s("F").s("me").v(1).v(2).text("p1").bool(true).text("q1").text("p2").bool(false).bool(true)
        })),
        // Material by reference, pattern by value.
        ("trim", Box::new(|b| b.v(5).v(0).s("minecraft:coast").text("Coast").bool(true))),
        ("debug_stick_state", Box::new(|b| b.raw(&compound()))),
        ("entity_data", Box::new(|b| b.v(7).raw(&compound()))),
        ("bucket_entity_data", Box::new(|b| b.raw(&compound()))),
        ("block_entity_data", Box::new(|b| b.v(2).raw(&compound()))),
        // A direct instrument: sound, use duration, range, durability damage, description.
        ("instrument", Box::new(|b| direct_sound(b.v(0)).f(7.0).f(256.0).v(1).text("Horn"))),
        ("provides_trim_material", Box::new(|b| b.v(0).s("minecraft:gold").text("Gold"))),
        ("ominous_bottle_amplifier", Box::new(|b| b.v(3))),
        ("jukebox_playable", Box::new(|b| direct_sound(b.v(0)).text("Song").f(120.0).v(5))),
        ("provides_banner_patterns", Box::new(|b| b.v(3).v(1).v(2))),
        ("recipes", Box::new(|b| b.raw(&[9, 0, 0, 0, 0, 0]))),
        ("lodestone_tracker", Box::new(|b| b.bool(true).s("minecraft:the_nether").l(0x1234).bool(false))),
        ("firework_explosion", Box::new(|b| b.v(4).v(2).i(1).i(2).v(1).i(3).bool(true).bool(false))),
        ("fireworks", Box::new(|b| b.v(3).v(1).v(0).v(0).v(0).bool(false).bool(true))),
        ("note_block_sound", Box::new(|b| b.s("minecraft:block.note_block.bell"))),
        ("banner_patterns", Box::new(|b| b.v(2).v(32).v(14).v(0).s("minecraft:x").s("y").v(11))),
        ("base_color", Box::new(|b| b.v(5))),
        ("pot_decorations", Box::new(|b| {
            b.bool(true).item("brick").v(1).v(0).v(0).bool(false).bool(true).item("angler_pottery_sherd").v(1).v(0).v(0).bool(false)
        })),
        // Three slots: stone, nothing, a damaged and renamed sword.
        ("container", Box::new(|b| {
            b.v(3).bool(true).item("stone").v(3).v(0).v(0).bool(false).bool(true).item("diamond_sword").v(1).v(2).v(0)
                .c("damage").v(5).c("custom_name").text("inner")
        })),
        ("block_state", Box::new(|b| b.v(2).s("facing").s("north").s("half").s("top"))),
        ("bees", Box::new(|b| b.v(1).v(9).raw(&compound()).v(5).v(10))),
        ("sulfur_cube_content", Box::new(|b| b.item("stone").v(1).v(0).v(0))),
        ("lock", Box::new(|b| b.raw(&compound()))),
        ("container_loot", Box::new(|b| b.raw(&compound()))),
        ("break_sound", Box::new(|b| b.v(40))),
        ("compostable", Box::new(|b| b.bool(true).i(3))),
        ("cooking_fuel", Box::new(|b| b.bool(true).i(200).bool(false).s("minecraft:fuel_speed"))),
        ("brewing_fuel", Box::new(|b| b.bool(false).s("minecraft:uses").bool(true).f(1.0))),
        ("mob_visibility", Box::new(|b| b.v(2).v(5).f(0.5))),
        ("wolf/variant", Box::new(|b| b.v(2))),
        ("cat/collar", Box::new(|b| b.v(14))),
        ("chicken/variant", Box::new(|b| b.v(1))),
        // A direct painting: width, height, asset, a title, no author.
        ("painting/variant", Box::new(|b| b.v(0).v(2).v(1).s("minecraft:x").bool(true).text("Title").bool(false))),
        ("provides_pottery_pattern", Box::new(|b| b.v(4))),
        ("sign_text_front", Box::new(|b| {
            b.text("a").text("b").text("c").text("d").bool(true).text("e").text("f").text("g").text("h").v(14).bool(true)
        })),
        ("sign_text_back", Box::new(|b| b.text("a").text("").text("").text("").bool(false).v(0).bool(false))),
        ("waxed", Box::new(|b| b)),
        ("cushion/color", Box::new(|b| b.v(3))),
    ];
    // The other mob variants and colours: registry ids.
    for name in [
        "villager/variant", "wolf/sound_variant", "wolf/collar", "fox/variant", "salmon/size", "parrot/variant",
        "tropical_fish/pattern", "tropical_fish/base_color", "tropical_fish/pattern_color", "mooshroom/variant",
        "rabbit/variant", "pig/variant", "pig/sound_variant", "cow/variant", "cow/sound_variant",
        "chicken/sound_variant", "zombie_nautilus/variant", "frog/variant", "horse/variant", "llama/variant",
        "axolotl/variant", "cat/variant", "cat/sound_variant", "sheep/color", "shulker/color",
    ] {
        list.push((name, Box::new(|b| b.v(200))));
    }
    list.retain(|(name, _)| for_protocol(p).data_component_type.contains(name));
    let mut b = B::new(p).v(1).item("stick").v(list.len() as i32).v(0);
    for (name, value) in &list {
        b = value(b.c(name));
    }
    (b.data, list.iter().map(|(name, _)| name.to_string()).collect())
}

#[test]
fn every_kind_of_component_is_read_past_exactly() {
    let f = Fixture::new();
    let cx = f.cx(777);
    let (mut bytes, components) = everything(777);
    let first = bytes.len();
    // A second slot right behind it must still decode.
    bytes.extend(B::new(777).v(2).item("stone").v(1).v(0).c("damage").v(4).data);
    let mut r = Reader::new(&bytes);
    let stack = read_slot(&mut r, &cx).unwrap().unwrap();
    assert_eq!(r.pos, first);
    assert_eq!(stack.item, "stick");
    assert_eq!(stack.components, components);
    // All of 26.3's components but the eight that decode into fields (covered above).
    assert_eq!(components.len(), for_protocol(777).data_component_type.len() - 8, "{components:?}");
    // Values nested in the skipped ones don't leak into the stack itself.
    assert_eq!((stack.damage, stack.custom_name.as_deref()), (None, None));
    let second = read_slot(&mut r, &cx).unwrap().unwrap();
    assert_eq!((second.item.as_str(), second.count, second.damage), ("stone", 2, Some(4)));
    assert_eq!(r.remaining(), 0);
    // Every component of every supported version either decodes or is read past.
    for p in [774, 775, 776, 777] {
        let cx = f.cx(p);
        for name in for_protocol(p).data_component_type {
            let err = Decoder::new(&cx).skip(name, &mut Reader::new(&[]), 0).err();
            let err = err.map(|e| e.to_string()).unwrap_or_default();
            assert!(!err.contains("unsupported"), "{p} {name}: {err}");
        }
    }
}

#[test]
fn every_truncation_is_an_error_and_no_input_panics() {
    let f = Fixture::new();
    let cx = f.cx(777);
    let (bytes, _) = everything(777);
    for end in 0..bytes.len() {
        assert!(read_slot(&mut Reader::new(&bytes[..end]), &cx).is_err(), "cut at {end}");
    }
    // Arbitrary bytes, and the fixture with one byte changed: errors or stacks, never a panic or
    // a runaway allocation.
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for _ in 0..3000 {
        let len = (next() % 64) as usize;
        let junk: Vec<u8> = (0..len).map(|_| next() as u8).collect();
        let _ = read_slot(&mut Reader::new(&junk), &cx);
        let mut mutated = bytes.clone();
        let at = (next() as usize) % mutated.len();
        mutated[at] = next() as u8;
        let _ = read_slot(&mut Reader::new(&mutated), &cx);
    }
}

#[test]
fn unknown_and_malformed_components_are_errors_naming_them() {
    let f = Fixture::new();
    let past_the_table = for_protocol(777).data_component_type.len() as i32;
    let err = f.err(777, &B::new(777).v(1).v(1).v(1).v(0).v(past_the_table).data);
    assert!(err.contains(&format!("unknown item component id {past_the_table}")), "{err}");
    // A bad value names the component (and the one holding it).
    let err = f.err(777, &B::new(777).v(1).v(1).v(1).v(0).c("consumable").f(1.0).v(0).v(1).bool(false).v(1).v(9).data);
    assert!(err.contains("item component consumable: unknown consume effect type 9"), "{err}");
    let err = f.err(777, &B::new(777).v(1).v(1).v(1).v(0).c("container").v(1).bool(true).v(1).v(1).v(1).v(0).c("repairable").v(-1).data);
    assert!(err.contains("item component container: item component repairable: negative holder set size"), "{err}");
    // Counts past the end of the data are refused before anything is allocated for them.
    let err = f.err(777, &B::new(777).v(1).v(1).v(i32::MAX).v(0).data);
    assert!(err.contains("bad count"), "{err}");
    let err = f.err(777, &B::new(777).v(1).v(1).v(1).v(0).c("lore").v(1_000_000).data);
    assert!(err.contains("item component lore: bad count"), "{err}");
}

/// A stack holding bundles `depth` deep, each the only stack in the one around it.
fn nested_bundles(depth: usize) -> Vec<u8> {
    let bundle = item_id(777, "bundle");
    let mut bytes = B::new(777).v(1).v(bundle).v(1).v(0).c("bundle_contents").v(1).data;
    for _ in 1..depth {
        // Each inner level is a template: item, then count.
        bytes.extend(B::new(777).v(bundle).v(1).v(1).v(0).c("bundle_contents").v(1).data);
    }
    bytes.extend(B::new(777).item("stone").v(1).v(0).v(0).data);
    bytes
}

/// Reads `bytes` as a slot on a thread with a 2 MiB stack (Rust's default for spawned threads).
fn on_small_stack(bytes: Vec<u8>) -> Result<Option<ItemStack>, String> {
    std::thread::Builder::new()
        .stack_size(2 << 20)
        .spawn(move || {
            let f = Fixture::new();
            read_slot(&mut Reader::new(&bytes), &f.cx(777)).map_err(|e| e.to_string())
        })
        .unwrap()
        .join()
        .unwrap()
}

#[test]
fn nesting_is_limited_instead_of_overflowing_the_stack() {
    // The deepest nesting allowed fits a 2 MiB stack (in a debug build, too).
    let stack = on_small_stack(nested_bundles(MAX_DEPTH as usize)).unwrap().unwrap();
    assert_eq!(stack.components, names(&["bundle_contents"]));
    let err = on_small_stack(nested_bundles(MAX_DEPTH as usize + 1)).unwrap_err();
    assert!(err.contains("nested too deeply"), "{err}");
    // Far deeper than any stack could take.
    let err = on_small_stack(nested_bundles(200_000)).unwrap_err();
    assert!(err.contains("nested too deeply"), "{err}");
    let f = Fixture::new();
    // A potion effect hiding an effect hiding another, 200,000 times over.
    let mut potion = B::new(777).v(1).v(1).v(1).v(0).c("potion_contents").bool(false).bool(false).v(1).v(5).data;
    potion.extend([0, 1, 0, 0, 0, 1].repeat(200_000));
    potion.extend([0, 1, 0, 0, 0, 0, 0]);
    assert!(f.err(777, &potion).contains("nested too deeply"));
}

#[test]
fn vanilla_26_3_encodings() {
    let f = Fixture::new();
    // Captured or encoded by vanilla 26.3 (Horizon's test fixtures): a slot from a real
    // ClientboundContainerSetSlotPacket with custom data and a direct trim; trims, banner patterns,
    // pot sherds, stew effects and a goat horn by reference; attribute modifiers, equippables, a
    // profile and a container.
    for (hex_bytes, item, components) in [
        (
            "01bd080200000a0300046b6565700000011c003a000e6d696e6563726166743a676f6c6408000f496e6c696e65206d6174657269616c000f6d696e6563726166743a636f61737408000e496e6c696e65207061747465726e00",
            "diamond_chestplate",
            &["custom_data", "trim"][..],
        ),
        ("01bd0801003a0502", "diamond_chestplate", &["trim"]),
        ("01890b01004a02200e000b6d696e6563726166743a7801790b", "white_banner", &["banner_patterns"]),
        ("01880301004c01f60801000001be0c0100000001d30c010000", "decorated_pot", &["pot_decorations"]),
        ("01a408010037020f6412a001", "stick", &["suspicious_stew_effects"]),
        ("01bd0801003f02", "diamond_chestplate", &["instrument"]),
        (
            "01a40801001002030b6d696e6563726166743a783ff80000000000000001001a0b6d696e6563726166743a793fd0000000000000020000",
            "stick",
            &["attribute_modifiers"],
        ),
        ("01a40801002003470000000101010000a80b", "stick", &["equippable"]),
        (
            "01a4080100200448010e6d696e6563726166743a676f6c64011a6d696e6563726166743a6d6973632f70756d706b696e626c75720103659a010000000100a80b",
            "stick",
            &["equippable"],
        ),
        (
            "01a40801002004000e6d696e6563726166743a7465737401408000000000000101010000a80b",
            "stick",
            &["equippable"],
        ),
        (
            "01a40803001001010b6d696e6563726166743a7a400800000000000000000003112003470000000101010000a80b",
            "stick",
            &["attribute_modifiers", "damage", "equippable"],
        ),
        ("01a40801004800010b486f72697a6f6e57697265000000000000", "stick", &["profile"]),
        ("01a40801004d0601010300000000000001d903020000", "stick", &["container"]),
    ] {
        let stack = f.one(777, &hex(hex_bytes)).unwrap();
        assert_eq!(stack.item, item, "{hex_bytes}");
        assert_eq!(stack.components, names(components), "{hex_bytes}");
        if components == ["profile"] {
            assert_eq!(stack.profile_name.as_deref(), Some("HorizonWire"));
        }
        if components.contains(&"damage") {
            assert_eq!(stack.damage, Some(17));
        }
    }
}

#[test]
fn the_26_1_and_26_2_layouts() {
    let f = Fixture::new();
    for p in [775, 776] {
        // pot_decorations: up to four item ids; a direct instrument without durability damage; a
        // direct trim material as an asset group; teleport_randomly without its particle flag;
        // swing_animation and map_color still exist.
        let bytes = B::new(p)
            .v(1)
            .item("decorated_pot")
            .v(7)
            .v(0)
            .c("pot_decorations")
            .v(2)
            .item("brick")
            .item("angler_pottery_sherd")
            .c("instrument")
            .v(0)
            .v(3)
            .f(7.0)
            .f(256.0)
            .text("Horn")
            .c("trim")
            .v(0)
            .s("gold")
            .v(1)
            .s("minecraft:gold")
            .s("gold_darker")
            .text("Gold")
            .v(2)
            .c("consumable")
            .f(1.6)
            .v(1)
            .v(5)
            .bool(true)
            .v(1)
            .v(3)
            .f(16.0)
            .c("swing_animation")
            .v(1)
            .v(6)
            .c("map_color")
            .i(0xff00)
            .c("damage")
            .v(9)
            .data;
        let stack = f.one(p, &bytes).unwrap();
        assert_eq!(stack.item, "decorated_pot");
        assert_eq!(
            stack.components,
            names(&["pot_decorations", "instrument", "trim", "consumable", "swing_animation", "map_color", "damage"])
        );
        assert_eq!(stack.damage, Some(9));
        // More than four sides is refused, as vanilla does.
        let err = f.err(p, &B::new(p).v(1).v(1).v(1).v(0).c("pot_decorations").v(5).raw(&[1; 5]).data);
        assert!(err.contains("5 pot decorations"), "{err}");
    }
    // A 26.2 bundle is a list of templates, as in 26.3; 26.2's numbering differs from 26.3's.
    let bytes = B::new(776).v(1).item("bundle").v(1).v(0).c("bundle_contents").v(1).item("diamond").v(3).v(0).v(0).data;
    assert_eq!(f.one(776, &bytes).unwrap().components, names(&["bundle_contents"]));
    assert_ne!(component_id(776, "bundle_contents"), component_id(777, "bundle_contents"));
}

#[test]
fn the_1_21_11_layout() {
    let f = Fixture::new();
    // Horizon's 1.21.11 fixtures, in 1.21.11's numbering. A filled map with map_color (gone in
    // 26.3), swing_animation, damage and a tooltip_display; it removes map_color and
    // potion_contents.
    let map = [1, 0xd0, 0x08, 4, 2, 43, 0x00, 0xff, 0x00, 0x00, 40, 1, 6, 3, 5, 18, 0, 2, 42, 40, 43, 49];
    let stack = f.one(774, &map).unwrap();
    assert_eq!(stack.item, "filled_map");
    assert_eq!(stack.components, names(&["map_color", "swing_animation", "damage", "tooltip_display"]));
    assert_eq!(stack.removed, names(&["map_color", "potion_contents"]));
    assert_eq!(stack.damage, Some(5));
    // A decorated pot: pot_decorations as item ids (a brick, an angler sherd), damage_resistant a
    // tag id, jukebox_playable an EitherHolder holding a song by reference...
    let mut pot = vec![1, 0xbf, 0x02, 3, 0, 72, 2, 0x81, 0x08, 0xa5, 0x0b, 27, 17];
    pot.extend(b"minecraft:is_fire");
    pot.extend([62, 1, 3]);
    let stack = f.one(774, &pot).unwrap();
    assert_eq!(stack.item, "decorated_pot");
    assert_eq!(stack.components, names(&["pot_decorations", "damage_resistant", "jukebox_playable"]));
    // ... or by its registry key.
    let mut song = vec![1, 0xbf, 0x02, 1, 0, 62, 0, 13];
    song.extend(b"minecraft:cat");
    assert_eq!(f.one(774, &song).unwrap().components, names(&["jukebox_playable"]));
    // EitherHolders: instrument, provides_trim_material, damage_type, chicken/variant,
    // zombie_nautilus/variant held, then an instrument by key.
    let held = B::new(774)
        .v(1)
        .v(1)
        .v(6)
        .v(0)
        .c("instrument")
        .raw(&[1, 2])
        .c("provides_trim_material")
        .raw(&[1, 5])
        .c("damage_type")
        .raw(&[1, 3])
        .c("chicken/variant")
        .raw(&[1, 2])
        .c("zombie_nautilus/variant")
        .raw(&[1, 0])
        .c("instrument")
        .raw(&[0, 26])
        .raw(b"minecraft:ponder_goat_horn")
        .data;
    assert_eq!(f.one(774, &held).unwrap().components.len(), 6);
    // A shulker box: container lists optional count-first ItemStacks (two diamonds with a
    // map_color, then nothing), use_remainder one ItemStack (a bowl).
    let shulker = [
        1, 0xc5, 0x04, 2, 0, 73, 2, 2, 0x82, 0x07, 1, 0, 43, 0, 0, 0, 5, 0, 25, 1, 0xfc, 0x06, 0, 0,
    ];
    let stack = f.one(774, &shulker).unwrap();
    assert_eq!((stack.item.as_str(), stack.components.clone()), ("shulker_box", names(&["container", "use_remainder"])));
    // Bundles and loaded crossbows hold count-first ItemStacks too (ItemStack.STREAM_CODEC in the
    // 1.21.11 jar), which may not be empty.
    let bundle = B::new(774).v(1).item("bundle").v(1).v(0).c("bundle_contents").v(2).v(2).item("stone").v(0).v(0);
    let bundle = bundle.v(1).item("arrow").v(1).v(0).c("damage").v(1).data;
    assert_eq!(f.one(774, &bundle).unwrap().components, names(&["bundle_contents"]));
    let empty = B::new(774).v(1).item("crossbow").v(1).v(0).c("charged_projectiles").v(1).v(0).data;
    assert!(f.err(774, &empty).contains("empty item stack"));
    // A shield: bypassed_by is a damage type tag; a banner pattern item names a tag; a direct
    // trim material is an asset group.
    let shield = B::new(774)
        .v(1)
        .item("shield")
        .v(3)
        .v(0)
        .c("blocks_attacks")
        .f(0.25)
        .f(1.0)
        .v(0)
        .f(3.0)
        .f(1.0)
        .f(1.0)
        .bool(true)
        .s("minecraft:bypasses_shield")
        .bool(false)
        .bool(false)
        .c("provides_banner_patterns")
        .s("minecraft:pattern_item/flower")
        .c("trim")
        .v(0)
        .s("gold")
        .v(0)
        .text("Gold")
        .v(3)
        .data;
    assert_eq!(f.one(774, &shield).unwrap().components, names(&["blocks_attacks", "provides_banner_patterns", "trim"]));
}

#[test]
fn the_layout_follows_the_registries() {
    assert_eq!(Layout::of(for_protocol(777)), Layout::V26_3);
    assert_eq!(Layout::of(for_protocol(776)), Layout::V26_1);
    assert_eq!(Layout::of(for_protocol(775)), Layout::V26_1);
    assert_eq!(Layout::of(for_protocol(774)), Layout::V1_21_11);
}

#[test]
fn the_deepest_items_and_nbt_fit_a_small_stack() {
    // Bundles as deep as allowed, the innermost holding stone whose custom_data nests compounds
    // as deep as the NBT reader allows (512).
    let stone = B::new(777).item("stone").v(1).v(0).v(0).data;
    let mut bytes = nested_bundles(MAX_DEPTH as usize - 1);
    bytes.truncate(bytes.len() - stone.len());
    bytes.extend(B::new(777).item("stone").v(1).v(1).v(0).c("custom_data").data);
    bytes.push(10);
    for _ in 0..511 {
        bytes.extend([10, 0, 1, b'a']);
    }
    bytes.extend(std::iter::repeat_n(0, 512));
    let stack = on_small_stack(bytes).unwrap().unwrap();
    assert_eq!(stack.item, "bundle");
}
