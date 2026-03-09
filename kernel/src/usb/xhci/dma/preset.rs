use crate::dma::DmaConstraints;

use super::profile::XhciDmaProfile;

const ALIGN_64: usize = 64;
const BOUNDARY_64K: usize = 64 * 1024;

pub(super) fn ring(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, ALIGN_64, BOUNDARY_64K)
}

pub(super) fn context(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, ALIGN_64, profile.page_size())
}

pub(super) fn dcbaa(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, ALIGN_64, profile.page_size())
}

pub(super) fn scratchpad_array(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, ALIGN_64, 0)
}

pub(super) fn scratchpad_buffer(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, profile.page_size(), 0)
}

pub(super) fn erst(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, ALIGN_64, 0)
}

pub(super) fn data_buffer(profile: &XhciDmaProfile) -> DmaConstraints {
    constraints(profile, 1, BOUNDARY_64K)
}

fn constraints(profile: &XhciDmaProfile, alignment: usize, boundary: usize) -> DmaConstraints {
    DmaConstraints {
        alignment,
        boundary,
        max_address: profile.max_address(),
        contiguous: true,
        zeroed: true,
    }
}
