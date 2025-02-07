// Copyright 2022 IOTA Stiftung
// SPDX-License-Identifier: Apache-2.0

//! Module containing the [`Signature`] type.

use iota_types::block::signature as iota;
use serde::{Deserialize, Serialize};

use crate::types::util::bytify;

/// Represents a signature used to unlock an output.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Signature {
    /// An [`Ed25519`](https://en.wikipedia.org/wiki/EdDSA) signature.
    Ed25519 {
        /// The public key as bytes.
        #[serde(with = "bytify")]
        public_key: [u8; Self::PUBLIC_KEY_LENGTH],
        /// The signature as bytes.
        #[serde(with = "bytify")]
        signature: [u8; Self::SIGNATURE_LENGTH],
    },
}

impl Signature {
    const PUBLIC_KEY_LENGTH: usize = iota::Ed25519Signature::PUBLIC_KEY_LENGTH;
    const SIGNATURE_LENGTH: usize = iota::Ed25519Signature::SIGNATURE_LENGTH;
}

impl From<&iota::Signature> for Signature {
    fn from(value: &iota::Signature) -> Self {
        match value {
            iota::Signature::Ed25519(signature) => Self::Ed25519 {
                public_key: *signature.public_key(),
                signature: *signature.signature(),
            },
        }
    }
}

impl From<Signature> for iota::Signature {
    fn from(value: Signature) -> Self {
        match value {
            Signature::Ed25519 { public_key, signature } => {
                iota::Signature::Ed25519(iota::Ed25519Signature::new(public_key, signature))
            }
        }
    }
}

impl From<Signature> for iota::dto::SignatureDto {
    fn from(value: Signature) -> Self {
        match value {
            Signature::Ed25519 { public_key, signature } => Self::Ed25519(iota::dto::Ed25519SignatureDto {
                kind: iota::Ed25519Signature::KIND,
                public_key: prefix_hex::encode(public_key),
                signature: prefix_hex::encode(signature),
            }),
        }
    }
}

#[cfg(feature = "rand")]
mod rand {
    use iota_types::block::rand::bytes::rand_bytes_array;

    use super::*;

    impl Signature {
        /// Generates a random [`Signature`] with an [`iota::Ed25519Signature`].
        pub fn rand() -> Self {
            Self::from(&iota::Signature::Ed25519(iota::Ed25519Signature::new(
                rand_bytes_array(),
                rand_bytes_array(),
            )))
        }
    }
}

#[cfg(all(test, feature = "rand"))]
mod test {
    use mongodb::bson::{from_bson, to_bson};

    use super::*;

    #[test]
    fn test_signature_bson() {
        let signature = Signature::rand();
        let bson = to_bson(&signature).unwrap();
        assert_eq!(signature, from_bson::<Signature>(bson).unwrap());
    }
}
