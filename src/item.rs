//! Item stacks as the server sends them (`ItemStack.OPTIONAL_STREAM_CODEC`): a count, the item's
//! registry id and a data component patch. The components a bot cares about are decoded (names,
//! lore, enchantments, durability, a head's profile); every other one is read past exactly as
//! vanilla wrote it ([`skip`]).
//!
//! Item data has no length prefix: a component read one byte short or long makes everything after
//! it (the rest of the stack, the rest of the window) unreadable. So a component this module does
//! not know is an error naming it, never a guess.
//!
//! Components are dispatched by name (`minecraft:data_component_type` of the connection's
//! protocol), so renumbering between versions costs nothing. The value layouts follow vanilla 26.3
//! (protocol 777); the few that differ in 26.1/26.2 (775/776) and 1.21.11 (774) are read in that
//! version's layout ([`Layout`]). Those differences were taken from the vanilla jars' stream
//! codecs (and Horizon's version adapters):
//!
//! - before 26.3: `pot_decorations` is up to four item ids, a direct `instrument` has no
//!   durability damage, a direct trim material is a `MaterialAssetGroup` (an asset suffix and its
//!   per-equipment overrides) and `teleport_randomly` has no particle flag; `swing_animation`
//!   (later split in two) and `map_color` exist;
//! - 1.21.11 also: `damage_type`, `instrument`, `provides_trim_material`, `jukebox_playable`,
//!   `chicken/variant` and `zombie_nautilus/variant` are `EitherHolder`s (a flag, then the holder
//!   or a registry key); `damage_resistant`, `provides_banner_patterns` and a shield's
//!   `bypassed_by` are a tag id rather than a holder set; and stacks inside components
//!   (`container`, `bundle_contents`, `charged_projectiles`, `use_remainder`) are count-first
//!   `ItemStack`s rather than item-first `ItemStackTemplate`s.

use crate::nbt::{self, Tag};
use crate::net::{Reader, invalid};
use crate::registries::{Dynamic, Registries, for_protocol};
use crate::text::Lang;
use std::io;

mod skip;
#[cfg(test)]
mod tests;

/// What decoding needs from the connection: the protocol's built-in registries, the data-driven
/// registries the server sent in configuration, and the language table for text components.
pub struct ItemContext<'a> {
    pub registries: &'a Registries,
    pub dynamic: &'a Dynamic,
    pub lang: &'a Lang,
}

/// One non-empty stack, reduced to what a bot reads off an item.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ItemStack {
    /// The item's name without namespace ("diamond_sword"); `#<id>` if unknown.
    pub item: String,
    pub count: i32,
    /// `minecraft:custom_name`, as plain text.
    pub custom_name: Option<String>,
    /// `minecraft:item_name`, as plain text.
    pub item_name: Option<String>,
    /// `minecraft:lore` lines, as plain text.
    pub lore: Vec<String>,
    /// `minecraft:enchantments`: the enchantment's name (from the server's `enchantment`
    /// registry, without `minecraft:`) or `#<id>`, and the level.
    pub enchantments: Vec<(String, i32)>,
    pub damage: Option<i32>,
    pub max_damage: Option<i32>,
    pub unbreakable: bool,
    /// The name in `minecraft:profile` (player heads), when it has one.
    pub profile_name: Option<String>,
    /// Names of all added components, in wire order, without namespace.
    pub components: Vec<String>,
    /// Names of the removed components.
    pub removed: Vec<String>,
}

impl ItemStack {
    /// The name a player sees: the custom name, else the item name component.
    pub fn display_name(&self) -> Option<&str> {
        self.custom_name.as_deref().or(self.item_name.as_deref())
    }

    /// A compact JSON object: `item` and `count`, then only the fields that are set.
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::{Map, Value, json};
        let mut o = Map::new();
        o.insert("item".into(), json!(self.item));
        o.insert("count".into(), json!(self.count));
        let mut text = |key: &str, v: &Option<String>| {
            if let Some(v) = v {
                o.insert(key.into(), json!(v));
            }
        };
        text("custom_name", &self.custom_name);
        text("item_name", &self.item_name);
        text("profile_name", &self.profile_name);
        if !self.lore.is_empty() {
            o.insert("lore".into(), json!(self.lore));
        }
        if !self.enchantments.is_empty() {
            let e: Map<String, Value> =
                self.enchantments.iter().map(|(name, level)| (name.clone(), json!(level))).collect();
            o.insert("enchantments".into(), Value::Object(e));
        }
        if let Some(d) = self.damage {
            o.insert("damage".into(), json!(d));
        }
        if let Some(d) = self.max_damage {
            o.insert("max_damage".into(), json!(d));
        }
        if self.unbreakable {
            o.insert("unbreakable".into(), json!(true));
        }
        if !self.components.is_empty() {
            o.insert("components".into(), json!(self.components));
        }
        if !self.removed.is_empty() {
            o.insert("removed".into(), json!(self.removed));
        }
        Value::Object(o)
    }
}

/// ItemStack.OPTIONAL_STREAM_CODEC: VarInt count (<= 0 => None), VarInt item id,
/// VarInt added count, VarInt removed count, added (VarInt type + value)..., removed (VarInt type)...
pub fn read_slot(r: &mut Reader, cx: &ItemContext) -> io::Result<Option<ItemStack>> {
    Decoder::new(cx).optional_stack(r, 0)
}

/// How deep values may nest while one stack is read: stacks in a stack's components (a bundle in a
/// shulker box in a bundle...), a mob effect's hidden effect, a block predicate's component
/// matchers. The codecs recurse without a limit of their own and a stack overflow would abort the
/// process; real items stay far below this.
const MAX_DEPTH: u32 = 64;

/// The item format generation a protocol speaks (see the module docs for what differs).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Layout {
    /// 1.21.11 (774).
    V1_21_11,
    /// 26.1 and 26.2 (775, 776).
    V26_1,
    /// 26.3 (777).
    V26_3,
}

impl Layout {
    /// The layout of the protocol whose component table `registries` holds (26.3 when it is none of
    /// the known ones).
    fn of(registries: &Registries) -> Self {
        let table = registries.data_component_type;
        let is = |protocol| {
            let known = for_protocol(protocol).data_component_type;
            std::ptr::eq(known, table) || known == table
        };
        if is(777) {
            Layout::V26_3
        } else if is(776) || is(775) {
            Layout::V26_1
        } else if is(774) {
            Layout::V1_21_11
        } else {
            Layout::V26_3
        }
    }
}

struct Decoder<'c, 'a> {
    cx: &'c ItemContext<'a>,
    layout: Layout,
}

impl<'c, 'a> Decoder<'c, 'a> {
    fn new(cx: &'c ItemContext<'a>) -> Self {
        Self { cx, layout: Layout::of(cx.registries) }
    }

    /// One more level of nesting, or an error past [`MAX_DEPTH`].
    fn enter(depth: u32) -> io::Result<u32> {
        if depth >= MAX_DEPTH {
            return Err(invalid("item data nested too deeply"));
        }
        Ok(depth + 1)
    }

    /// ItemStack.OPTIONAL_STREAM_CODEC.
    fn optional_stack(&self, r: &mut Reader, depth: u32) -> io::Result<Option<ItemStack>> {
        let count = r.varint()?;
        if count <= 0 {
            return Ok(None);
        }
        let item = r.varint()?;
        self.finish(item, count, r, depth).map(Some)
    }

    /// ItemStack.STREAM_CODEC: the optional form, which must not be empty.
    fn stack(&self, r: &mut Reader, depth: u32) -> io::Result<ItemStack> {
        self.optional_stack(r, depth)?.ok_or_else(|| invalid("empty item stack where one is required"))
    }

    /// ItemStackTemplate.STREAM_CODEC: the item first, then the count and the patch.
    fn template(&self, r: &mut Reader, depth: u32) -> io::Result<ItemStack> {
        let item = r.varint()?;
        let count = r.varint()?;
        self.finish(item, count, r, depth)
    }

    /// A stack held by a component value (`use_remainder`, `bundle_contents`, ...): a template
    /// since 26.1, an ItemStack before.
    fn nested(&self, r: &mut Reader, depth: u32) -> io::Result<ItemStack> {
        if self.layout == Layout::V1_21_11 { self.stack(r, depth) } else { self.template(r, depth) }
    }

    fn finish(&self, item: i32, count: i32, r: &mut Reader, depth: u32) -> io::Result<ItemStack> {
        let mut stack = ItemStack {
            item: Registries::name(self.cx.registries.item, item),
            count,
            ..ItemStack::default()
        };
        self.patch(r, &mut stack, depth)?;
        Ok(stack)
    }

    /// DataComponentPatch.STREAM_CODEC: both counts, the added components' types and values, then
    /// the removed components' types.
    fn patch(&self, r: &mut Reader, stack: &mut ItemStack, depth: u32) -> io::Result<()> {
        // Every entry takes at least a byte, so neither count can pass what is left.
        let added = skip::count(r)?;
        let removed = skip::count(r)?;
        for _ in 0..added {
            let name = self.component_name(r.varint()?)?;
            stack.components.push(name.to_owned());
            self.component(name, r, stack, depth)?;
        }
        for _ in 0..removed {
            let id = r.varint()?;
            stack.removed.push(Registries::name(self.cx.registries.data_component_type, id));
        }
        Ok(())
    }

    fn component_name(&self, id: i32) -> io::Result<&'static str> {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.cx.registries.data_component_type.get(i).copied())
            .ok_or_else(|| invalid(format!("unknown item component id {id}")))
    }

    /// One added component's value: the interesting ones into `stack`, the rest read past.
    fn component(&self, name: &str, r: &mut Reader, stack: &mut ItemStack, depth: u32) -> io::Result<()> {
        let depth = Self::enter(depth)?;
        self.value(name, r, stack, depth)
            .map_err(|e| invalid(format!("item component {name}: {e}")))
    }

    fn value(&self, name: &str, r: &mut Reader, s: &mut ItemStack, depth: u32) -> io::Result<()> {
        match name {
            "custom_name" => s.custom_name = Some(self.text(r)?),
            "item_name" => s.item_name = Some(self.text(r)?),
            "lore" => {
                let n = skip::count(r)?;
                for _ in 0..n {
                    let line = self.text(r)?;
                    s.lore.push(line);
                }
            }
            // ItemEnchantments: enchantment registry ids and levels.
            "enchantments" => {
                let n = skip::count(r)?;
                for _ in 0..n {
                    let id = r.varint()?;
                    let level = r.varint()?;
                    s.enchantments.push((self.dynamic_name("enchantment", id), level));
                }
            }
            "damage" => s.damage = Some(r.varint()?),
            "max_damage" => s.max_damage = Some(r.varint()?),
            "unbreakable" => s.unbreakable = true,
            "profile" => s.profile_name = profile(r)?,
            other => self.skip(other, r, depth)?,
        }
        Ok(())
    }

    /// A text component (ComponentSerialization.STREAM_CODEC: network NBT) as plain text.
    fn text(&self, r: &mut Reader) -> io::Result<String> {
        Ok(nbt::read_network(r)?.map_or_else(String::new, |t: Tag| crate::text::plain(&t, self.cx.lang)))
    }

    /// An entry of a registry the server sent, without `minecraft:`, or `#<id>`.
    fn dynamic_name(&self, registry: &str, id: i32) -> String {
        usize::try_from(id)
            .ok()
            .and_then(|i| self.cx.dynamic.get(registry)?.get(i))
            .map_or_else(
                || format!("#{id}"),
                |name| name.strip_prefix("minecraft:").unwrap_or(name).to_owned(),
            )
    }
}

/// ResolvableProfile.STREAM_CODEC: Either<GameProfile (UUID, name, properties), Partial (optional
/// name, optional UUID, properties)>, then PlayerSkin.Patch (three optional texture ids and an
/// optional model). The name, when there is a non-empty one.
fn profile(r: &mut Reader) -> io::Result<Option<String>> {
    let name = if r.bool()? {
        r.uuid()?;
        Some(r.string()?)
    } else {
        let name = if r.bool()? { Some(r.string()?) } else { None };
        skip::optional(r, |r| r.uuid().map(drop))?;
        name
    };
    // Properties: name, value, optional signature.
    skip::list(r, |r| {
        skip::string(r)?;
        skip::string(r)?;
        skip::optional(r, skip::string)
    })?;
    for _ in 0..3 {
        skip::optional(r, skip::string)?;
    }
    skip::optional(r, skip::boolean)?;
    Ok(name.filter(|n| !n.is_empty()))
}
