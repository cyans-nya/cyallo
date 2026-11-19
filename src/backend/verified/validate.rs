//! Structural validation against the exact immutable preparation obligations.

use super::{
    Architecture, MachineRange, NativeTier, Origin, PreparedFunction, Relocation, UnlinkedImage,
};
use crate::bytecode::verified::InstructionId;
use std::{collections::HashSet, fmt, sync::Arc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageError {
    WrongPlan,
    EmptyImage,
    InvalidRange(MachineRange),
    OverlappingRange(MachineRange),
    UnpreparedInstruction(InstructionId),
    MissingInstruction(InstructionId),
    RelocationCount { expected: usize, actual: usize },
    InvalidRelocation(Relocation),
    MachineInfoMismatch,
    InvalidMachineOffset(u32),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPlan => f.write_str("native image belongs to a different prepared function"),
            Self::EmptyImage => f.write_str("native image contains no code"),
            Self::InvalidRange(range) => {
                write!(f, "invalid native range {}..{}", range.start, range.end)
            }
            Self::OverlappingRange(range) => write!(
                f,
                "native range {}..{} overlaps an earlier range or is out of order",
                range.start, range.end
            ),
            Self::UnpreparedInstruction(pc) => {
                write!(f, "native image names unprepared instruction {}", pc.0)
            }
            Self::MissingInstruction(pc) => {
                write!(f, "native image omits prepared instruction {}", pc.0)
            }
            Self::RelocationCount { expected, actual } => {
                write!(f, "expected {expected} linkage relocations, found {actual}")
            }
            Self::InvalidRelocation(relocation) => write!(
                f,
                "invalid linkage relocation at instruction {}",
                relocation.instruction.0
            ),
            Self::MachineInfoMismatch => {
                f.write_str("native image omitted or misidentified its machine inspection data")
            }
            Self::InvalidMachineOffset(offset) => {
                write!(f, "invalid machine metadata offset {offset}")
            }
        }
    }
}

impl std::error::Error for ImageError {}

/// Validated code and exact source and linkage obligations.
#[derive(Debug)]
pub struct ValidatedImage(UnlinkedImage);

impl ValidatedImage {
    pub fn validate(
        expected: &Arc<PreparedFunction>,
        image: UnlinkedImage,
    ) -> Result<Self, ImageError> {
        // Function IDs and code versions are scoped. Equality of their numeric
        // values does not identify the prepared object or its obligations.
        if !Arc::ptr_eq(expected, &image.plan) {
            return Err(ImageError::WrongPlan);
        }
        if image.bytes.is_empty() {
            return Err(ImageError::EmptyImage);
        }
        let alignment = match expected.target().architecture {
            Architecture::Aarch64 => size_of::<u32>(),
            Architecture::X86_64 => 1,
        };
        match (image.tier, &image.cranelift) {
            (NativeTier::Cranelift(_), Some(info)) if !info.blocks.is_empty() => {
                for offset in info
                    .blocks
                    .iter()
                    .copied()
                    .chain(info.edges.iter().flat_map(|&(from, to)| [from, to]))
                    .chain(info.traps.iter().map(|trap| trap.offset))
                {
                    if !usize::try_from(offset).is_ok_and(|offset| {
                        offset < image.bytes.len() && offset.is_multiple_of(alignment)
                    }) {
                        return Err(ImageError::InvalidMachineOffset(offset));
                    }
                }
            }
            (NativeTier::Winch, None) => {}
            _ => return Err(ImageError::MachineInfoMismatch),
        }
        let mut last_end = 0;
        let mut covered = HashSet::with_capacity(expected.sites().len());
        for &range in &image.ranges {
            if range.start >= range.end
                || [range.start, range.end].into_iter().any(|offset| {
                    !usize::try_from(offset).is_ok_and(|offset| {
                        offset <= image.bytes.len() && offset.is_multiple_of(alignment)
                    })
                })
            {
                return Err(ImageError::InvalidRange(range));
            }
            if range.start < last_end {
                return Err(ImageError::OverlappingRange(range));
            }
            if expected.site(range.instruction).is_none() {
                return Err(ImageError::UnpreparedInstruction(range.instruction));
            }
            covered.insert(range.instruction);
            last_end = range.end;
        }
        for site in expected.sites() {
            if !covered.contains(&site.origin.instruction) {
                return Err(ImageError::MissingInstruction(site.origin.instruction));
            }
        }
        let relocations = expected.relocations();
        if relocations.len() != image.relocations.len() {
            return Err(ImageError::RelocationCount {
                expected: relocations.len(),
                actual: image.relocations.len(),
            });
        }
        for (index, (expected, &actual)) in relocations.zip(&image.relocations).enumerate() {
            let offset = usize::try_from(actual.offset.0).ok();
            if actual != expected
                || offset != index.checked_mul(actual.kind.width())
                || actual.addend != 0
            {
                return Err(ImageError::InvalidRelocation(actual));
            }
        }
        Ok(Self(image))
    }

    pub fn image(&self) -> &UnlinkedImage {
        &self.0
    }

    pub fn origin_at(&self, machine_offset: u32) -> Option<Origin> {
        let index = self
            .0
            .ranges
            .partition_point(|range| range.start <= machine_offset)
            .checked_sub(1)?;
        let range = self.0.ranges.get(index)?;
        if machine_offset >= range.end {
            return None;
        }
        self.0.plan.site(range.instruction).map(|site| site.origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backend::verified::{CodeVersion, NativeTarget, NativeTier},
        bytecode::verified::VerifiedModule,
        compile,
    };

    #[test]
    fn image_validation_rejects_forged_ranges_and_stale_plan_identity() {
        let Some(target) = NativeTarget::host() else {
            return;
        };
        let program = compile("fn increment(value: i64) -> i64 { return value + 1 }\nfn entry(value: i64) -> i64 { return increment(value) }\n").unwrap();
        let id = program
            .functions()
            .iter()
            .position(|function| function.name.as_ref() == "entry")
            .unwrap();
        let module = VerifiedModule::lower(&program, id).unwrap();
        let prepare = || {
            Arc::new(
                PreparedFunction::prepare(&module, module.entry(), CodeVersion(1), target).unwrap(),
            )
        };
        let plan = prepare();
        let emit = || NativeTier::Winch.emit(plan.clone()).unwrap();
        assert_eq!(
            ValidatedImage::validate(&prepare(), emit()).unwrap_err(),
            ImageError::WrongPlan
        );

        let mut image = emit();
        image.ranges = Box::new([]);
        assert_eq!(
            ValidatedImage::validate(&plan, image).unwrap_err(),
            ImageError::MissingInstruction(plan.sites()[0].origin.instruction)
        );

        let mut image = emit();
        image.ranges[0].instruction = InstructionId(u32::MAX);
        assert_eq!(
            ValidatedImage::validate(&plan, image).unwrap_err(),
            ImageError::UnpreparedInstruction(InstructionId(u32::MAX))
        );

        let mut image = emit();
        image.ranges[0].end = u32::MAX;
        let bad_range = image.ranges[0];
        assert_eq!(
            ValidatedImage::validate(&plan, image).unwrap_err(),
            ImageError::InvalidRange(bad_range)
        );

        let mut image = emit();
        image.relocations[0].offset.0 += 1;
        let relocation = image.relocations[0];
        assert_eq!(
            ValidatedImage::validate(&plan, image).unwrap_err(),
            ImageError::InvalidRelocation(relocation)
        );

        let mut image = emit();
        image.relocations[0].target.version.0 += 1;
        let relocation = image.relocations[0];
        assert_eq!(
            ValidatedImage::validate(&plan, image).unwrap_err(),
            ImageError::InvalidRelocation(relocation)
        );
    }
}
