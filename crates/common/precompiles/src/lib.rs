#![doc = include_str!("../README.md")]
#![cfg_attr(not(test), warn(unused_crate_dependencies))]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

mod provider;
pub use provider::BasePrecompiles;

mod spec;
pub use spec::BasePrecompileSpec;

mod bn254_pair;
pub use bn254_pair::{JOVIAN, JOVIAN_MAX_INPUT_SIZE};

mod bls12_381;
pub use bls12_381::{
    JOVIAN_G1_MSM, JOVIAN_G1_MSM_MAX_INPUT_SIZE, JOVIAN_G2_MSM, JOVIAN_G2_MSM_MAX_INPUT_SIZE,
    JOVIAN_PAIRING, JOVIAN_PAIRING_MAX_INPUT_SIZE,
};
