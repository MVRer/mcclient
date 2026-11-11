//! Reading past the data components nobody interprets: each arm consumes exactly what the vanilla
//! stream codec writes (DataComponents' `networkSynchronized` codecs; a component without one goes
//! as its persistent codec's NBT). Ported from Horizon's `item/skip.rs` (26.3) and checked against
//! the vanilla 1.21.11, 26.1, 26.2 and 26.3 jars for the older layouts.

use super::{Decoder, ItemStack, Layout};
use crate::nbt;
use crate::net::{Reader, invalid};
use std::io;

impl Decoder<'_, '_> {
    /// Reads past the value of component `name` (a name from `data_component_type`). `depth` is
    /// the nesting of this value, for the components that hold stacks or recurse.
    pub(super) fn skip(&self, name: &str, r: &mut Reader, depth: u32) -> io::Result<()> {
        let old = self.layout == Layout::V1_21_11;
        let before_26_3 = self.layout < Layout::V26_3;
        match name {
            // Unit.STREAM_CODEC.
            "unbreakable" | "creative_slot_lock" | "glider" | "waxed" => Ok(()),
            // Network NBT: CustomData and the compounds (custom_data, bucket_entity_data), text
            // components, and components without a network codec, sent as their NBT
            // (intangible_projectile, map_decorations, debug_stick_state, recipes, lock,
            // container_loot).
            "custom_data" | "bucket_entity_data" | "custom_name" | "item_name" | "intangible_projectile"
            | "map_decorations" | "debug_stick_state" | "recipes" | "lock" | "container_loot" => nbt(r),
            "lore" => list(r, nbt),
            // VAR_INT, an id mapper or a registry id.
            "max_stack_size" | "max_damage" | "damage" | "rarity" | "repair_cost" | "enchantable"
            | "additional_trade_cost" | "block_transformer" | "villager_food" | "dye" | "map_id"
            | "map_post_processing" | "ominous_bottle_amplifier" | "base_color" | "provides_pottery_pattern" => {
                varint(r)
            }
            // The mob variants and colours: registry ids (holderRegistry) or id mappers.
            "villager/variant" | "wolf/variant" | "wolf/sound_variant" | "wolf/collar" | "fox/variant"
            | "salmon/size" | "parrot/variant" | "tropical_fish/pattern" | "tropical_fish/base_color"
            | "tropical_fish/pattern_color" | "mooshroom/variant" | "rabbit/variant" | "pig/variant"
            | "pig/sound_variant" | "cow/variant" | "cow/sound_variant" | "chicken/sound_variant"
            | "frog/variant" | "horse/variant" | "llama/variant" | "axolotl/variant" | "cat/variant"
            | "cat/sound_variant" | "cat/collar" | "sheep/color" | "shulker/color" | "cushion/color" => {
                varint(r)
            }
            // EitherHolder in 1.21.11 (a flag, then the registry id or a key); a registry id since.
            "damage_type" | "chicken/variant" | "zombie_nautilus/variant" => either_holder(r, old, varint),
            "minimum_attack_charge" | "potion_duration_scale" => f32(r),
            // INT: a dyed colour, a map's colour (before 26.3).
            "dyed_color" | "map_color" => i32(r),
            "enchantment_glint_override" => boolean(r),
            // Identifier.STREAM_CODEC.
            "item_model" | "tooltip_style" | "note_block_sound" => string(r),
            // UseEffects: canSprint, interactVibrations, speedMultiplier.
            "use_effects" => {
                boolean(r)?;
                boolean(r)?;
                f32(r)
            }
            // AdventureModePredicate: block predicates.
            "can_place_on" | "can_break" => list(r, |r| self.block_predicate(r, depth)),
            "attribute_modifiers" => list(r, attribute_modifier),
            // CustomModelData: floats, flags, strings, colours.
            "custom_model_data" => {
                list(r, f32)?;
                list(r, boolean)?;
                list(r, string)?;
                list(r, i32)
            }
            // TooltipDisplay: hideTooltip, hidden component types.
            "tooltip_display" => {
                boolean(r)?;
                list(r, varint)
            }
            // ItemEnchantments: enchantment id, level.
            "enchantments" | "stored_enchantments" => list(r, |r| {
                varint(r)?;
                varint(r)
            }),
            // FoodProperties: nutrition, saturation, canAlwaysEat.
            "food" => {
                varint(r)?;
                f32(r)?;
                boolean(r)
            }
            // Consumable: consumeSeconds, animation, sound, hasConsumeParticles, onConsumeEffects.
            "consumable" => {
                f32(r)?;
                varint(r)?;
                sound(r)?;
                boolean(r)?;
                list(r, |r| self.consume_effect(r, depth))
            }
            // UseRemainder: one stack.
            "use_remainder" => self.nested(r, depth).map(drop),
            // SulfurCubeContent (26.2+): one template.
            "sulfur_cube_content" => self.template(r, depth).map(drop),
            // UseCooldown: seconds, cooldownGroup.
            "use_cooldown" => {
                f32(r)?;
                optional(r, string)
            }
            // DamageResistant: a damage type tag in 1.21.11, a holder set since.
            "damage_resistant" => tag_or_holder_set(r, old),
            // Tool: rules (blocks, speed, correctForDrops), defaultMiningSpeed, damagePerBlock,
            // canDestroyBlocksInCreative.
            "tool" => {
                list(r, |r| {
                    holder_set(r)?;
                    optional(r, f32)?;
                    optional(r, boolean)
                })?;
                f32(r)?;
                varint(r)?;
                boolean(r)
            }
            // Weapon: itemDamagePerAttack, disableBlockingForSeconds.
            "weapon" => {
                varint(r)?;
                f32(r)
            }
            // AttackRange: six floats.
            "attack_range" => (0..6).try_for_each(|_| f32(r)),
            "equippable" => equippable(r),
            // Repairable: an item holder set.
            "repairable" => holder_set(r),
            // DeathProtection: deathEffects.
            "death_protection" => list(r, |r| self.consume_effect(r, depth)),
            "blocks_attacks" => blocks_attacks(r, old),
            // PiercingWeapon: dealsKnockback, dismounts, sound, hitSound.
            "piercing_weapon" => {
                boolean(r)?;
                boolean(r)?;
                optional(r, sound)?;
                optional(r, sound)
            }
            "kinetic_weapon" => kinetic_weapon(r),
            // SwingAnimation (before 26.3; split into these two since): type, duration.
            "swing_animation" | "attack_animation" | "interact_animation" => {
                varint(r)?;
                varint(r)
            }
            // ChargedProjectiles, BundleContents: stacks.
            "charged_projectiles" | "bundle_contents" => list(r, |r| self.nested(r, depth).map(drop)),
            // PotionContents: potion, customColor, customEffects, customName.
            "potion_contents" => {
                optional(r, varint)?;
                optional(r, i32)?;
                list(r, |r| mob_effect(r, depth))?;
                optional(r, string)
            }
            // SuspiciousStewEffects: effect, duration.
            "suspicious_stew_effects" => list(r, |r| {
                varint(r)?;
                varint(r)
            }),
            // WritableBookContent: Filterable<String> pages.
            "writable_book_content" => list(r, |r| filterable(r, string)),
            // WrittenBookContent: Filterable title, author, generation, Filterable<Component> pages,
            // resolved.
            "written_book_content" => {
                filterable(r, string)?;
                string(r)?;
                varint(r)?;
                list(r, |r| filterable(r, nbt))?;
                boolean(r)
            }
            // ArmorTrim: Holder<TrimMaterial>, Holder<TrimPattern> (assetId, description, decal).
            "trim" => {
                trim_material(r, before_26_3)?;
                holder(r, |r| {
                    string(r)?;
                    nbt(r)?;
                    boolean(r)
                })
            }
            // TypedEntityData: the type's id, then a compound.
            "entity_data" | "block_entity_data" => {
                varint(r)?;
                nbt(r)
            }
            // InstrumentComponent: Holder<Instrument> (soundEvent, useDuration, range,
            // durabilityDamage (26.3+), description).
            "instrument" => either_holder(r, old, |r| {
                holder(r, |r| {
                    sound(r)?;
                    f32(r)?;
                    f32(r)?;
                    if !before_26_3 {
                        varint(r)?;
                    }
                    nbt(r)
                })
            }),
            "provides_trim_material" => either_holder(r, old, |r| trim_material(r, before_26_3)),
            // JukeboxPlayable: Holder<JukeboxSong> (soundEvent, description, lengthInSeconds,
            // comparatorOutput).
            "jukebox_playable" => either_holder(r, old, |r| {
                holder(r, |r| {
                    sound(r)?;
                    nbt(r)?;
                    f32(r)?;
                    varint(r)
                })
            }),
            // A banner pattern tag in 1.21.11, a holder set since.
            "provides_banner_patterns" => tag_or_holder_set(r, old),
            // LodestoneTracker: target (GlobalPos: dimension, packed BlockPos), tracked.
            "lodestone_tracker" => {
                optional(r, |r| {
                    string(r)?;
                    i64(r)
                })?;
                boolean(r)
            }
            "firework_explosion" => firework_explosion(r),
            // Fireworks: flightDuration, explosions.
            "fireworks" => {
                varint(r)?;
                list(r, firework_explosion)
            }
            "profile" => super::profile(r).map(drop),
            // BannerPatternLayers: Holder<BannerPattern> (assetId, translationKey), colour.
            "banner_patterns" => list(r, |r| {
                holder(r, |r| {
                    string(r)?;
                    string(r)
                })?;
                varint(r)
            }),
            // PotDecorations: four optional templates (back, left, right, front) since 26.3; up to
            // four item ids (a brick for a plain side) before.
            "pot_decorations" if before_26_3 => {
                let n = count(r)?;
                if n > 4 {
                    return Err(invalid(format!("{n} pot decorations")));
                }
                (0..n).try_for_each(|_| varint(r))
            }
            "pot_decorations" => (0..4).try_for_each(|_| optional(r, |r| self.template(r, depth).map(drop))),
            // ItemContainerContents: up to 256 slots, each an optional template (an optional
            // count-first ItemStack in 1.21.11).
            "container" if old => list(r, |r| self.optional_stack(r, depth).map(drop)),
            "container" => list(r, |r| optional(r, |r| self.template(r, depth).map(drop))),
            // BlockItemStateProperties: a string to string map.
            "block_state" => list(r, |r| {
                string(r)?;
                string(r)
            }),
            // Bees: Occupant (TypedEntityData: type and compound, ticksInHive, minTicksInHive).
            "bees" => list(r, |r| {
                varint(r)?;
                nbt(r)?;
                varint(r)?;
                varint(r)
            }),
            "break_sound" => sound(r),
            // Compostable: layers.
            "compostable" => resolvable(r, i32),
            // CookingFuel (burnTime, speedMultiplier), BrewingFuel (uses, speedMultiplier).
            "cooking_fuel" | "brewing_fuel" => {
                resolvable(r, i32)?;
                resolvable(r, f32)
            }
            // MobVisibility: targetingEntityTypes, visibility.
            "mob_visibility" => {
                holder_set(r)?;
                f32(r)
            }
            // Holder<PaintingVariant>: width, height, assetId, title, author.
            "painting/variant" => holder(r, |r| {
                varint(r)?;
                varint(r)?;
                string(r)?;
                optional(r, nbt)?;
                optional(r, nbt)
            }),
            // SignText: four messages, four optional filtered ones, colour, hasGlowingText.
            "sign_text_front" | "sign_text_back" => {
                (0..4).try_for_each(|_| nbt(r))?;
                optional(r, |r| (0..4).try_for_each(|_| nbt(r)))?;
                varint(r)?;
                boolean(r)
            }
            other => Err(invalid(format!("unsupported item component {other}"))),
        }
    }

    /// ConsumeEffect.STREAM_CODEC: the consume_effect_type id, then that type's fields.
    fn consume_effect(&self, r: &mut Reader, depth: u32) -> io::Result<()> {
        match r.varint()? {
            // apply_effects: effects, probability.
            0 => {
                list(r, |r| mob_effect(r, depth))?;
                f32(r)
            }
            // remove_effects: a mob effect holder set.
            1 => holder_set(r),
            // clear_all_effects.
            2 => Ok(()),
            // teleport_randomly: diameter, then directionalParticles (26.3+).
            3 => {
                f32(r)?;
                if self.layout == Layout::V26_3 { boolean(r) } else { Ok(()) }
            }
            // play_sound.
            4 => sound(r),
            other => Err(invalid(format!("unknown consume effect type {other}"))),
        }
    }

    /// BlockPredicate: blocks, properties, nbt, then DataComponentMatchers (exact components,
    /// partial predicates).
    fn block_predicate(&self, r: &mut Reader, depth: u32) -> io::Result<()> {
        optional(r, holder_set)?;
        // StatePropertiesPredicate: PropertyMatchers, each a name and an Either<ExactMatcher
        // (value), RangedMatcher (min, max)>.
        optional(r, |r| {
            list(r, |r| {
                string(r)?;
                if r.bool()? {
                    string(r)
                } else {
                    optional(r, string)?;
                    optional(r, string)
                }
            })
        })?;
        optional(r, nbt)?;
        // DataComponentExactPredicate: typed components, each a type and its value.
        list(r, |r| {
            let name = self.component_name(r.varint()?)?;
            self.component(name, r, &mut ItemStack::default(), depth)
        })?;
        // DataComponentPredicate.STREAM_CODEC: Either<predicate type, component type> ids, then the
        // predicate as NBT.
        list(r, |r| {
            boolean(r)?;
            varint(r)?;
            nbt(r)
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Primitives and the generic ByteBufCodecs shapes.

/// A VarInt element count: never negative, and never more than the bytes left (every element
/// takes at least one), so nothing is allocated or looped for a bogus count.
pub(super) fn count(r: &mut Reader) -> io::Result<usize> {
    let n = r.varint()?;
    match usize::try_from(n) {
        Ok(n) if n <= r.remaining() => Ok(n),
        _ => Err(invalid(format!("bad count {n} with {} bytes left", r.remaining()))),
    }
}
fn varint(r: &mut Reader) -> io::Result<()> {
    r.varint().map(drop)
}
pub(super) fn boolean(r: &mut Reader) -> io::Result<()> {
    r.u8().map(drop)
}
fn f32(r: &mut Reader) -> io::Result<()> {
    r.take(4).map(drop)
}
fn i32(r: &mut Reader) -> io::Result<()> {
    r.take(4).map(drop)
}
fn i64(r: &mut Reader) -> io::Result<()> {
    r.take(8).map(drop)
}
/// STRING_UTF8, Identifier.STREAM_CODEC, ResourceKey.streamCodec, TagKey.streamCodec.
pub(super) fn string(r: &mut Reader) -> io::Result<()> {
    let n = r.prefixed_len()?;
    r.take(n).map(drop)
}
/// A network NBT value (text components, compounds, NBT-coded components).
fn nbt(r: &mut Reader) -> io::Result<()> {
    nbt::read_network(r).map(drop)
}
/// ByteBufCodecs.optional: a flag, then the value.
pub(super) fn optional(r: &mut Reader, value: impl FnOnce(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    if r.bool()? { value(r) } else { Ok(()) }
}
/// ByteBufCodecs.list / collection / map: a count, then the elements.
pub(super) fn list(r: &mut Reader, mut element: impl FnMut(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    let n = count(r)?;
    (0..n).try_for_each(|_| element(r))
}
/// Filterable.streamCodec: the raw value, then an optional filtered one.
fn filterable(r: &mut Reader, value: fn(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    value(r)?;
    optional(r, value)
}
/// ByteBufCodecs.holder: 0 and the value itself, or the registry id plus one (vanilla refuses a
/// negative one).
fn holder(r: &mut Reader, direct: impl FnOnce(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    match r.varint()? {
        0 => direct(r),
        n if n < 0 => Err(invalid(format!("negative holder id {n}"))),
        _ => Ok(()),
    }
}
/// ByteBufCodecs.holderSet: 0 and a tag id, or the size plus one and the registry ids.
fn holder_set(r: &mut Reader) -> io::Result<()> {
    match r.varint()? {
        0 => string(r),
        n if n < 0 => Err(invalid(format!("negative holder set size {n}"))),
        n => {
            let n = (n - 1) as usize;
            if n > r.remaining() {
                return Err(invalid(format!("holder set of {n} past the end")));
            }
            (0..n).try_for_each(|_| varint(r))
        }
    }
}
/// A TagKey (1.21.11) or a HolderSet (26.1+).
fn tag_or_holder_set(r: &mut Reader, tag: bool) -> io::Result<()> {
    if tag { string(r) } else { holder_set(r) }
}
/// EitherHolder.streamCodec (1.21.11): `either(holder, ResourceKey)`, true for the holder; the
/// bare holder from 26.1.
fn either_holder(r: &mut Reader, either: bool, holder: impl FnOnce(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    if !either || r.bool()? { holder(r) } else { string(r) }
}
/// SoundEvent.STREAM_CODEC: a holder of a location and an optional fixed range.
fn sound(r: &mut Reader) -> io::Result<()> {
    holder(r, |r| {
        string(r)?;
        optional(r, f32)
    })
}
/// ResolvableInt, ResolvableFloat: Either a constant (true) or a context provider's key.
fn resolvable(r: &mut Reader, constant: fn(&mut Reader) -> io::Result<()>) -> io::Result<()> {
    if r.bool()? { constant(r) } else { string(r) }
}

// ---------------------------------------------------------------------------------------------
// Component value shapes.

/// TrimMaterial.STREAM_CODEC: a holder of a direct material: its asset (26.3: one palette id;
/// before: a MaterialAssetGroup, a base suffix and an equipment-asset to suffix map) and its
/// description.
fn trim_material(r: &mut Reader, asset_group: bool) -> io::Result<()> {
    holder(r, |r| {
        string(r)?;
        if asset_group {
            list(r, |r| {
                string(r)?;
                string(r)
            })?;
        }
        nbt(r)
    })
}

/// MobEffectInstance.STREAM_CODEC: the effect's registry id, then its Details.
fn mob_effect(r: &mut Reader, depth: u32) -> io::Result<()> {
    varint(r)?;
    effect_details(r, depth)
}

/// MobEffectInstance.Details: amplifier, duration, ambient, showParticles, showIcon, then the
/// optional hidden effect's Details, recursively.
fn effect_details(r: &mut Reader, depth: u32) -> io::Result<()> {
    let depth = Decoder::enter(depth)?;
    varint(r)?;
    varint(r)?;
    boolean(r)?;
    boolean(r)?;
    boolean(r)?;
    optional(r, |r| effect_details(r, depth))
}

/// ItemAttributeModifiers.Entry: attribute, modifier (id, amount, operation), slot group, display
/// (default, hidden, or override and its text).
fn attribute_modifier(r: &mut Reader) -> io::Result<()> {
    varint(r)?;
    string(r)?;
    i64(r)?;
    varint(r)?;
    varint(r)?;
    match r.varint()? {
        0 | 1 => Ok(()),
        2 => nbt(r),
        other => Err(invalid(format!("unknown attribute modifier display {other}"))),
    }
}

/// Equippable: slot, equipSound, assetId, cameraOverlay, allowedEntities, dispensable, swappable,
/// damageOnHurt, equipOnInteract, canBeSheared, shearingSound.
fn equippable(r: &mut Reader) -> io::Result<()> {
    varint(r)?;
    sound(r)?;
    optional(r, string)?;
    optional(r, string)?;
    optional(r, holder_set)?;
    (0..5).try_for_each(|_| boolean(r))?;
    sound(r)
}

/// BlocksAttacks: blockDelaySeconds, disableCooldownScale, damageReductions (angle, type, base,
/// factor), itemDamage (threshold, base, factor), bypassedBy (a tag in 1.21.11, a holder set
/// since), blockSound, disableSound.
fn blocks_attacks(r: &mut Reader, tag: bool) -> io::Result<()> {
    f32(r)?;
    f32(r)?;
    list(r, |r| {
        f32(r)?;
        optional(r, holder_set)?;
        f32(r)?;
        f32(r)
    })?;
    (0..3).try_for_each(|_| f32(r))?;
    optional(r, |r| tag_or_holder_set(r, tag))?;
    optional(r, sound)?;
    optional(r, sound)
}

/// KineticWeapon: contactCooldownTicks, delayTicks, the dismount, knockback and damage conditions
/// (maxDurationTicks, minSpeed, minRelativeSpeed), forwardMovement, damageMultiplier, sound,
/// hitSound.
fn kinetic_weapon(r: &mut Reader) -> io::Result<()> {
    varint(r)?;
    varint(r)?;
    (0..3).try_for_each(|_| {
        optional(r, |r| {
            varint(r)?;
            f32(r)?;
            f32(r)
        })
    })?;
    f32(r)?;
    f32(r)?;
    optional(r, sound)?;
    optional(r, sound)
}

/// FireworkExplosion: shape, colors, fadeColors, hasTrail, hasTwinkle.
fn firework_explosion(r: &mut Reader) -> io::Result<()> {
    varint(r)?;
    list(r, i32)?;
    list(r, i32)?;
    boolean(r)?;
    boolean(r)
}
