//! Borrowed DOS MZ / LE / LX header inspection; no loading or execution of LE/LX images.
#![no_std]
#![forbid(unsafe_code)]
mod fixup;
mod le;
mod mz;
pub use fixup::{
    LeFixup, LeFixupError, LeFixupErrorKind, LeFixupField, LeFixupLimits, LeFixupSourceType,
    LeFixupSources, LeFixupTarget, LeFixupUnsupported, LeFixups,
};
pub use le::{
    LeError, LeHeader, LeImage, LeLimit, LeLimits, LeLocation, LeObject, LeObjectOffset, LePage,
    LePageData, LeResource,
};
pub use mz::{ExecutableError, ExtendedHeader, MzHeader, MzImage, Relocation};
