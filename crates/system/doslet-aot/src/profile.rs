//! Bounded native speed-tier JSON and conservative guidance for existing code.
//!
//! Profile PCs are linear addresses in one explicitly supplied layout. Neither
//! identity strings nor observed byte windows establish code or reachability.
//! Binding compares only real CFG instructions, including the exact relocation
//! bytes from the supplied plan. Static IRQ labels describe observations, not
//! actual interrupt residency. Labels and counts are optimization metadata.

use alloc::{string::String, vec::Vec};
use core::{fmt, mem::size_of};
use serde::{
    Deserialize,
    de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor},
};

use crate::{DefaultBitness, ObjectAddress, binding::DosFlatBindingPlan, cfg::StaticCfg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProfileLimits {
    pub max_input_bytes: usize,
    /// Checked before parsing/appending another node, including duplicates.
    pub max_nodes: usize,
    pub max_identity_bytes: usize,
    /// Requested node-vector and identity storage; parser scratch is separately
    /// bounded by max_input_bytes. Allocator bookkeeping is excluded.
    pub max_owned_bytes: usize,
}
impl Default for ProfileLimits {
    fn default() -> Self {
        Self {
            max_input_bytes: 64 * 1024 * 1024,
            max_nodes: 100_000,
            max_identity_bytes: 4096,
            max_owned_bytes: 32 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
pub enum Tier {
    #[serde(rename = "fast", alias = "hot")]
    Hot,
    #[serde(rename = "slow", alias = "cold")]
    Cold,
    #[default]
    #[serde(rename = "grey")]
    Grey,
    #[serde(rename = "irq")]
    Irq,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Reason {
    Manual,
    CodeConflict,
    InterruptFrame,
    Learned,
    Observed,
    #[default]
    Other,
}
impl<'de> Deserialize<'de> for Reason {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ReasonVisitor;
        impl Visitor<'_> for ReasonVisitor {
            type Value = Reason;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a reason string")
            }
            fn visit_str<E: de::Error>(self, s: &str) -> Result<Reason, E> {
                Ok(match s {
                    "manual" => Reason::Manual,
                    "code-conflict" => Reason::CodeConflict,
                    "interrupt-frame" => Reason::InterruptFrame,
                    "learned" => Reason::Learned,
                    "observed" => Reason::Observed,
                    _ => Reason::Other,
                })
            }
        }
        deserializer.deserialize_str(ReasonVisitor)
    }
}

/// Optional observed frequencies. Duplicate records use maxima, never sums;
/// these values do not represent work executed by the generated program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Weight {
    pub count: Option<u64>,
    pub high_count: Option<u64>,
}
impl Weight {
    fn merge(&mut self, other: Self) {
        self.count = self.count.max(other.count);
        self.high_count = self.high_count.max(other.high_count);
    }
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileNode {
    pub pc: u32,
    #[serde(deserialize_with = "bitness")]
    pub bits: DefaultBitness,
    pub bytes: [u8; 15],
    pub tier: Tier,
    #[serde(default)]
    pub reason: Reason,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub high_count: Option<u64>,
}
fn bitness<'de, D: de::Deserializer<'de>>(d: D) -> Result<DefaultBitness, D::Error> {
    match u32::deserialize(d)? {
        16 => Ok(DefaultBitness::Bits16),
        32 => Ok(DefaultBitness::Bits32),
        _ => Err(de::Error::custom("profile bits must be 16 or 32")),
    }
}
fn key(node: &ProfileNode) -> (u32, u8) {
    (
        node.pc,
        match node.bits {
            DefaultBitness::Bits16 => 16,
            DefaultBitness::Bits32 => 32,
        },
    )
}

#[derive(Debug)]
pub struct Profile {
    identity: Option<String>,
    nodes: Vec<ProfileNode>,
    pub rates: Option<[Option<u16>; 3]>,
    pub learning: Option<bool>,
    pub learned_format: bool,
}
impl Profile {
    pub fn parse(input: &[u8], limits: ProfileLimits) -> Result<Self, ProfileError> {
        if input.len() > limits.max_input_bytes {
            return Err(ProfileError::InputLimit);
        }
        let mut failure = None;
        let mut budget = Budget(limits.max_owned_bytes);
        let mut decoder = serde_json::Deserializer::from_slice(input);
        let result = ProfileSeed {
            limits,
            budget: &mut budget,
            failure: &mut failure,
        }
        .deserialize(&mut decoder);
        let mut profile = match result {
            Ok(profile) => profile,
            Err(error) => return Err(failure.unwrap_or(ProfileError::Json(error))),
        };
        decoder.end().map_err(ProfileError::Json)?;
        profile.nodes.sort_unstable_by_key(key);
        Ok(profile)
    }
    /// Caller-visible native identity, not a hash computed from this image.
    pub fn identity(&self) -> Option<&str> {
        self.identity.as_deref()
    }
    pub fn nodes(&self) -> &[ProfileNode] {
        &self.nodes
    }
}

#[derive(Debug)]
pub enum ProfileError {
    InputLimit,
    NodeLimit,
    IdentityLimit,
    OwnedByteLimit,
    Allocation,
    /// Exactly one supported version field is required.
    Version {
        version: Option<u32>,
        learning_version: Option<u32>,
    },
    Policy,
    Json(serde_json::Error),
}
impl fmt::Display for ProfileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "speed-tier JSON: {error}"),
            other => write!(f, "speed-tier profile: {other:?}"),
        }
    }
}
impl core::error::Error for ProfileError {}

struct ProfileSeed<'a> {
    limits: ProfileLimits,
    budget: &'a mut Budget,
    failure: &'a mut Option<ProfileError>,
}
impl<'de> DeserializeSeed<'de> for ProfileSeed<'_> {
    type Value = Profile;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Profile, D::Error> {
        d.deserialize_map(self)
    }
}
#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    Version,
    LearningVersion,
    Identity,
    Unknown,
    IrqPolicy,
    Rates,
    Learning,
    Nodes,
}
#[derive(Deserialize)]
enum IrqPolicy {
    #[serde(rename = "static-grey")]
    StaticGrey,
}
fn fail<E: de::Error>(slot: &mut Option<ProfileError>, error: ProfileError) -> E {
    *slot = Some(error);
    E::custom("speed-tier profile validation failed")
}
impl<'de> Visitor<'de> for ProfileSeed<'_> {
    type Value = Profile;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a speed-tier profile object")
    }
    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Profile, M::Error> {
        let (
            mut version,
            mut learned_version,
            mut identity,
            mut unknown,
            mut irq_policy,
            mut rates,
            mut learning,
            mut nodes,
        ) = (None, None, None, None, None, None, None, None);
        let mut seen = 0u16;
        while let Some(field) = map.next_key::<Field>()? {
            let bit = 1
                << match field {
                    Field::Version => 0,
                    Field::LearningVersion => 1,
                    Field::Identity => 2,
                    Field::Unknown => 3,
                    Field::IrqPolicy => 4,
                    Field::Rates => 5,
                    Field::Learning => 6,
                    Field::Nodes => 7,
                };
            if seen & bit != 0 {
                return Err(de::Error::custom("duplicate profile field"));
            }
            seen |= bit;
            match field {
                Field::Version => version = Some(map.next_value()?),
                Field::LearningVersion => learned_version = Some(map.next_value()?),
                Field::Identity => {
                    identity = Some(map.next_value_seed(IdentitySeed {
                        limits: self.limits,
                        budget: self.budget,
                        failure: self.failure,
                    })?)
                }
                Field::Unknown => unknown = Some(map.next_value::<Tier>()?),
                Field::IrqPolicy => irq_policy = Some(map.next_value::<IrqPolicy>()?),
                Field::Rates => rates = Some(map.next_value()?),
                Field::Learning => learning = Some(map.next_value()?),
                Field::Nodes => {
                    nodes = Some(map.next_value_seed(NodesSeed {
                        limits: self.limits,
                        budget: self.budget,
                        failure: self.failure,
                    })?)
                }
            }
        }
        if !matches!(
            (version, learned_version),
            (Some(1), None) | (None, Some(1))
        ) {
            return Err(fail(
                self.failure,
                ProfileError::Version {
                    version,
                    learning_version: learned_version,
                },
            ));
        }
        if unknown != Some(Tier::Grey) || irq_policy.is_none() {
            return Err(fail(self.failure, ProfileError::Policy));
        }
        Ok(Profile {
            identity,
            nodes: nodes.ok_or_else(|| de::Error::missing_field("nodes"))?,
            rates,
            learning,
            learned_format: learned_version.is_some(),
        })
    }
}
struct IdentitySeed<'a> {
    limits: ProfileLimits,
    budget: &'a mut Budget,
    failure: &'a mut Option<ProfileError>,
}
impl<'de> DeserializeSeed<'de> for IdentitySeed<'_> {
    type Value = String;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<String, D::Error> {
        d.deserialize_str(self)
    }
}
impl Visitor<'_> for IdentitySeed<'_> {
    type Value = String;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an identity string")
    }
    fn visit_str<E: de::Error>(self, text: &str) -> Result<String, E> {
        if text.len() > self.limits.max_identity_bytes {
            return Err(fail(self.failure, ProfileError::IdentityLimit));
        }
        self.budget
            .charge(text.len())
            .map_err(|e| fail(self.failure, e))?;
        let mut identity = String::new();
        identity
            .try_reserve_exact(text.len())
            .map_err(|_| fail(self.failure, ProfileError::Allocation))?;
        identity.push_str(text);
        Ok(identity)
    }
}
struct NodesSeed<'a> {
    limits: ProfileLimits,
    budget: &'a mut Budget,
    failure: &'a mut Option<ProfileError>,
}
impl<'de> DeserializeSeed<'de> for NodesSeed<'_> {
    type Value = Vec<ProfileNode>;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
        d.deserialize_seq(self)
    }
}
impl<'de> Visitor<'de> for NodesSeed<'_> {
    type Value = Vec<ProfileNode>;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an array of profile nodes")
    }
    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> Result<Self::Value, S::Error> {
        let mut nodes = Vec::new();
        loop {
            let node = seq.next_element_seed(NodeSeed {
                full: nodes.len() == self.limits.max_nodes,
                failure: self.failure,
            })?;
            let Some(node) = node else { break };
            if nodes.len() == nodes.capacity() {
                let capacity = nodes
                    .capacity()
                    .saturating_mul(2)
                    .max(16)
                    .min(self.limits.max_nodes)
                    .min(
                        nodes
                            .capacity()
                            .saturating_add(self.budget.0 / size_of::<ProfileNode>()),
                    );
                if capacity <= nodes.len() {
                    return Err(fail(self.failure, ProfileError::OwnedByteLimit));
                }
                self.budget
                    .reserve_to(&mut nodes, capacity)
                    .map_err(|e| fail(self.failure, e))?;
            }
            nodes.push(node);
        }
        Ok(nodes)
    }
}
struct NodeSeed<'a> {
    full: bool,
    failure: &'a mut Option<ProfileError>,
}
impl<'de> DeserializeSeed<'de> for NodeSeed<'_> {
    type Value = ProfileNode;
    fn deserialize<D: de::Deserializer<'de>>(self, d: D) -> Result<ProfileNode, D::Error> {
        if self.full {
            return Err(fail(self.failure, ProfileError::NodeLimit));
        }
        ProfileNode::deserialize(d)
    }
}
struct Budget(usize);
impl Budget {
    fn charge(&mut self, bytes: usize) -> Result<(), ProfileError> {
        self.0 = self
            .0
            .checked_sub(bytes)
            .ok_or(ProfileError::OwnedByteLimit)?;
        Ok(())
    }
    fn reserve_to<T>(&mut self, values: &mut Vec<T>, capacity: usize) -> Result<(), ProfileError> {
        self.charge(
            capacity
                .saturating_sub(values.capacity())
                .checked_mul(size_of::<T>())
                .ok_or(ProfileError::OwnedByteLimit)?,
        )?;
        values
            .try_reserve_exact(capacity.saturating_sub(values.len()))
            .map_err(|_| ProfileError::Allocation)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Context {
    #[default]
    Unknown,
    IrqObserved,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GuidanceStatus {
    #[default]
    Unmatched,
    Verified,
    ByteMismatch,
    Conflict,
    ProfileCodeConflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InstructionGuidance {
    pub address: ObjectAddress,
    pub tier: Tier,
    pub context: Context,
    pub weight: Weight,
    pub status: GuidanceStatus,
}
impl InstructionGuidance {
    pub fn unknown(address: ObjectAddress) -> Self {
        Self {
            address,
            tier: Tier::Grey,
            context: Context::Unknown,
            weight: Weight::default(),
            status: GuidanceStatus::Unmatched,
        }
    }
    pub fn verified_hot(self) -> bool {
        self.tier == Tier::Hot
            && self.status == GuidanceStatus::Verified
            && self.context == Context::Unknown
    }
    #[cfg(feature = "wasm")]
    pub(crate) fn region_class(self) -> (Tier, Context) {
        (self.tier, self.context)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnmatchedReason {
    UnmappedAddress,
    NotInstructionStart,
    BitnessMismatch,
    ByteMismatch,
    ConflictingEvidence,
    ProfileCodeConflict,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuidanceDiagnostic {
    pub pc: u32,
    pub bits: DefaultBitness,
    pub reason: UnmatchedReason,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GuidanceReport {
    pub input_nodes: usize,
    pub unique_records: usize,
    pub duplicate_records: usize,
    pub conflicting_records: usize,
    pub matched_instructions: usize,
    pub unmatched_records: usize,
    pub byte_mismatches: usize,
    pub profile_code_conflicts: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityCheck {
    Unchecked,
    Matched,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuidanceLimits {
    pub max_owned_bytes: usize,
}
impl Default for GuidanceLimits {
    fn default() -> Self {
        Self {
            max_owned_bytes: 64 * 1024 * 1024,
        }
    }
}
#[derive(Debug)]
pub enum GuidanceError {
    DifferentImage,
    MissingIdentity,
    IdentityMismatch,
    Storage(ProfileError),
    Address,
}
impl fmt::Display for GuidanceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "speed-tier guidance: {self:?}")
    }
}
impl core::error::Error for GuidanceError {}
impl From<ProfileError> for GuidanceError {
    fn from(error: ProfileError) -> Self {
        Self::Storage(error)
    }
}

/// Immutable guidance tied to the exact CFG and explicit binding it verified.
#[derive(Debug)]
pub struct BoundGuidance<'cfg, 'binding, 'image> {
    graph: &'cfg StaticCfg<'image>,
    binding: &'binding DosFlatBindingPlan<'image>,
    instructions: Vec<InstructionGuidance>,
    diagnostics: Vec<GuidanceDiagnostic>,
    report: GuidanceReport,
    identity_check: IdentityCheck,
    rebased: bool,
}
#[derive(Clone, Copy)]
struct ObjectRange {
    base: u32,
    end: u64,
    number: u32,
}
impl<'cfg, 'binding, 'image> BoundGuidance<'cfg, 'binding, 'image> {
    pub fn bind(
        profile: &Profile,
        graph: &'cfg StaticCfg<'image>,
        binding: &'binding DosFlatBindingPlan<'image>,
        expected_identity: Option<&str>,
        limits: GuidanceLimits,
    ) -> Result<Self, GuidanceError> {
        if !core::ptr::eq(graph.image(), binding.image()) {
            return Err(GuidanceError::DifferentImage);
        }
        let identity_check = if let Some(expected) = expected_identity {
            let actual = profile.identity().ok_or(GuidanceError::MissingIdentity)?;
            if expected != actual {
                return Err(GuidanceError::IdentityMismatch);
            }
            IdentityCheck::Matched
        } else {
            IdentityCheck::Unchecked
        };
        let mut budget = Budget(limits.max_owned_bytes);
        let mut objects = Vec::new();
        budget.reserve_to(&mut objects, graph.image().objects().len())?;
        for object in graph.image().objects() {
            // Empty objects may legally share a base or sit inside another
            // object's extent. They cannot contain a profile PC and must not
            // hide the preceding nonempty range in this search index.
            if object.bytes().is_empty() {
                continue;
            }
            let binding = binding
                .object_binding(object.object_number())
                .ok_or(GuidanceError::Address)?;
            objects.push(ObjectRange {
                base: binding.linear_base,
                end: u64::from(binding.linear_base) + object.bytes().len() as u64,
                number: object.object_number(),
            });
        }
        objects.sort_unstable_by_key(|object| object.base);
        let mut result = Self {
            graph,
            binding,
            instructions: Vec::new(),
            diagnostics: Vec::new(),
            report: GuidanceReport {
                input_nodes: profile.nodes.len(),
                ..GuidanceReport::default()
            },
            identity_check,
            rebased: false,
        };
        budget.reserve_to(&mut result.instructions, graph.nodes().len())?;
        result.instructions.extend(
            graph
                .nodes()
                .iter()
                .map(|node| InstructionGuidance::unknown(node.address)),
        );
        budget.reserve_to(&mut result.diagnostics, profile.nodes.len())?;
        let mut start = 0;
        while let Some(first) = profile.nodes.get(start) {
            let mut end = start + 1;
            while profile
                .nodes
                .get(end)
                .is_some_and(|node| key(node) == key(first))
            {
                end += 1;
            }
            let group = &profile.nodes[start..end];
            result.report.unique_records += 1;
            result.report.duplicate_records += group.len() - 1;
            result.bind_group(group, &objects)?;
            start = end;
        }
        Ok(result)
    }
    pub fn instructions(&self) -> &[InstructionGuidance] {
        &self.instructions
    }
    /// Carry verified object-relative labels to another explicit layout of the
    /// exact same immutable image. Original verification still requires exact
    /// historical relocation bytes; this never masks mismatches or adds code.
    /// Diagnostics retain the source profile's historical PCs. The target
    /// runtime must independently validate its current code and segment state.
    pub fn rebind_to<'target>(
        &self,
        binding: &'target DosFlatBindingPlan<'image>,
        limits: GuidanceLimits,
    ) -> Result<BoundGuidance<'cfg, 'target, 'image>, GuidanceError> {
        if !core::ptr::eq(self.graph.image(), binding.image()) {
            return Err(GuidanceError::DifferentImage);
        }
        let mut budget = Budget(limits.max_owned_bytes);
        let mut instructions = Vec::new();
        let mut diagnostics = Vec::new();
        budget.reserve_to(&mut instructions, self.instructions.len())?;
        budget.reserve_to(&mut diagnostics, self.diagnostics.len())?;
        instructions.extend_from_slice(&self.instructions);
        diagnostics.extend_from_slice(&self.diagnostics);
        Ok(BoundGuidance {
            graph: self.graph,
            binding,
            instructions,
            diagnostics,
            report: self.report,
            identity_check: self.identity_check,
            rebased: true,
        })
    }

    /// True after explicitly carrying historical proof to a target binding.
    pub fn is_rebased(&self) -> bool {
        self.rebased
    }
    pub fn diagnostics(&self) -> &[GuidanceDiagnostic] {
        &self.diagnostics
    }
    pub fn report(&self) -> GuidanceReport {
        self.report
    }
    pub fn identity_check(&self) -> IdentityCheck {
        self.identity_check
    }
    pub fn matches(&self, graph: &StaticCfg<'_>, binding: &DosFlatBindingPlan<'_>) -> bool {
        core::ptr::eq(self.graph, graph) && core::ptr::eq(self.binding, binding)
    }
    fn diagnose(&mut self, node: &ProfileNode, reason: UnmatchedReason) {
        self.diagnostics.push(GuidanceDiagnostic {
            pc: node.pc,
            bits: node.bits,
            reason,
        });
    }
    fn bind_group(
        &mut self,
        group: &[ProfileNode],
        objects: &[ObjectRange],
    ) -> Result<(), GuidanceError> {
        let first = &group[0];
        let object = objects
            .partition_point(|object| object.base <= first.pc)
            .checked_sub(1)
            .and_then(|index| objects.get(index))
            .filter(|object| u64::from(first.pc) < object.end);
        let unresolved = if let Some(object) = object {
            let address = ObjectAddress {
                object_number: object.number,
                offset: first.pc - object.base,
            };
            match self
                .graph
                .nodes()
                .binary_search_by_key(&address, |node| node.address)
            {
                Ok(index) if self.graph.nodes()[index].bitness == first.bits => {
                    return self.bind_instruction(group, index);
                }
                Ok(_) => UnmatchedReason::BitnessMismatch,
                Err(_) => UnmatchedReason::NotInstructionStart,
            }
        } else {
            UnmatchedReason::UnmappedAddress
        };
        self.report.unmatched_records += 1;
        self.diagnose(first, unresolved);
        if group.iter().skip(1).any(|row| {
            row.tier != first.tier || context(row) != context(first) || row.bytes != first.bytes
        }) {
            self.report.conflicting_records += 1;
            self.diagnose(first, UnmatchedReason::ConflictingEvidence);
        }
        Ok(())
    }
    fn bind_instruction(
        &mut self,
        group: &[ProfileNode],
        index: usize,
    ) -> Result<(), GuidanceError> {
        let first = &group[0];
        let node = &self.graph.nodes()[index];
        let length = usize::from(node.byte_len);
        let start = usize::try_from(node.address.offset).map_err(|_| GuidanceError::Address)?;
        let object = self
            .graph
            .image()
            .object(node.address.object_number)
            .ok_or(GuidanceError::Address)?;
        let original = object
            .bytes()
            .get(start..start + length)
            .ok_or(GuidanceError::Address)?;
        let mut actual = [0; 15];
        actual[..length].copy_from_slice(original);
        let patches = self
            .binding
            .object_patches(node.address.object_number)
            .ok_or(GuidanceError::Address)?;
        let first_patch = patches.partition_point(|patch| {
            u64::from(patch.source.offset) + patch.bytes().len() as u64
                <= u64::from(node.address.offset)
        });
        for patch in &patches[first_patch..] {
            let patch_start =
                usize::try_from(patch.source.offset).map_err(|_| GuidanceError::Address)?;
            if patch_start >= start + length {
                break;
            }
            for (index, &byte) in patch.bytes().iter().enumerate() {
                let at = patch_start + index;
                if at >= start && at - start < length {
                    actual[at - start] = byte;
                }
            }
        }
        let mut value = InstructionGuidance::unknown(node.address);
        value.tier = first.tier;
        let mut mismatch = false;
        let mut conflict = false;
        let mut code_conflict = false;
        let initial_context = context(first);
        for row in group {
            value.weight.merge(Weight {
                count: row.count,
                high_count: row.high_count,
            });
            let row_context = context(row);
            if row_context == Context::IrqObserved {
                value.context = row_context;
            }
            mismatch |= row.bytes[..length] != actual[..length];
            conflict |= row.tier != first.tier
                || row_context != initial_context
                || row.bytes[..length] != first.bytes[..length];
            code_conflict |= row.reason == Reason::CodeConflict;
        }
        let issue = if conflict {
            self.report.conflicting_records += 1;
            value.status = GuidanceStatus::Conflict;
            Some(UnmatchedReason::ConflictingEvidence)
        } else if code_conflict {
            self.report.profile_code_conflicts += 1;
            value.status = GuidanceStatus::ProfileCodeConflict;
            Some(UnmatchedReason::ProfileCodeConflict)
        } else if mismatch {
            value.status = GuidanceStatus::ByteMismatch;
            Some(UnmatchedReason::ByteMismatch)
        } else {
            value.status = GuidanceStatus::Verified;
            if value.context == Context::IrqObserved {
                value.tier = Tier::Irq;
            }
            self.report.matched_instructions += 1;
            None
        };
        if mismatch {
            self.report.byte_mismatches += 1;
        }
        if let Some(issue) = issue {
            value.tier = Tier::Grey;
            self.report.unmatched_records += 1;
            self.diagnose(first, issue);
        }
        self.instructions[index] = value;
        Ok(())
    }
}
fn context(node: &ProfileNode) -> Context {
    if node.tier == Tier::Irq || node.reason == Reason::InterruptFrame {
        Context::IrqObserved
    } else {
        Context::Unknown
    }
}

#[cfg(test)]
mod tests;
