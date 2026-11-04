use crate::{
    config::Config,
    error::{Error, Result},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use solana_pubkey::Pubkey;
use std::str::FromStr;

pub const TOKEN: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const BOOKING_LEN: usize = 520;
pub const TERMS_LEN: usize = 132;
pub const MAX_ACCOUNT_BYTES: usize = 10 * 1024 * 1024;
pub const LEGACY_TERMS_DISCRIMINATOR: [u8; 8] = [223, 24, 40, 223, 249, 219, 14, 97];
pub const LEGACY_BOOKING_DISCRIMINATOR: [u8; 8] = [147, 50, 61, 138, 208, 21, 254, 156];
pub const MARKET_DISCRIMINATOR: [u8; 8] = [219, 190, 213, 55, 0, 227, 198, 154];
pub const MODEL_DISCRIMINATOR: [u8; 8] = [152, 221, 247, 122, 185, 125, 223, 151];
pub const MODEL_GROUP_DISCRIMINATOR: [u8; 8] = [237, 175, 79, 136, 212, 228, 240, 177];
pub const PROVIDER_DISCRIMINATOR: [u8; 8] = [164, 180, 71, 17, 75, 216, 80, 195];
pub const MODELMETADATA_DISCRIMINATOR: [u8; 8] = [243, 161, 2, 132, 123, 88, 51, 61];
pub const LISTMETADATA_DISCRIMINATOR: [u8; 8] = [245, 77, 238, 20, 114, 17, 94, 226];
pub const TERMS_DISCRIMINATOR: [u8; 8] = [136, 206, 216, 233, 146, 248, 21, 65];
pub const BOOKING_TERMS_DISCRIMINATOR: [u8; 8] = [7, 171, 147, 70, 168, 59, 177, 8];
pub const BOOKING_POLICIES_DISCRIMINATOR: [u8; 8] = [63, 176, 147, 212, 151, 136, 53, 228];
pub const WALLET_BOOKINGS_DISCRIMINATOR: [u8; 8] = [251, 175, 236, 250, 115, 241, 109, 202];
pub const PROVIDER_POLICY_DISCRIMINATOR: [u8; 8] = [160, 80, 153, 123, 160, 152, 122, 32];
pub const PROVIDER_CAPACITY_DISCRIMINATOR: [u8; 8] = [139, 242, 225, 128, 103, 72, 36, 45];
pub const LIST_ROUTING_DISCRIMINATOR: [u8; 8] = [144, 176, 101, 27, 253, 191, 157, 242];
pub const LIST_TERMS_DISCRIMINATOR: [u8; 8] = [174, 240, 243, 180, 203, 181, 0, 107];
pub const LIST_CAPACITY_DISCRIMINATOR: [u8; 8] = [73, 4, 213, 31, 149, 94, 219, 141];
pub const PROVIDER_WALLET_DISCRIMINATOR: [u8; 8] = [89, 4, 132, 220, 207, 78, 178, 52];
pub const PROVIDER_LIMIT_PAGE_DISCRIMINATOR: [u8; 8] = [18, 101, 138, 157, 3, 31, 233, 252];
pub const PROVIDER_LIMIT_INDEX_DISCRIMINATOR: [u8; 8] = [94, 216, 132, 219, 87, 112, 166, 89];
pub const CAPACITY_DISCRIMINATOR: [u8; 8] = [19, 216, 196, 142, 39, 29, 145, 16];
pub const USER_DISCRIMINATOR: [u8; 8] = [159, 117, 95, 227, 239, 151, 58, 236];
pub const BOOKING_DISCRIMINATOR: [u8; 8] = [151, 160, 101, 76, 250, 167, 210, 104];
pub const LIST_DISCRIMINATOR: [u8; 8] = [169, 24, 186, 110, 22, 139, 190, 82];
pub const RESERVE_BOOKING_DISCRIMINATOR: [u8; 8] = [170, 28, 99, 26, 95, 11, 225, 109];
pub const RESERVE_LIST_BOOKING_DISCRIMINATOR: [u8; 8] = [170, 22, 43, 144, 39, 255, 146, 208];
pub const CLAIM_LIST_BOOKING_DISCRIMINATOR: [u8; 8] = [50, 202, 193, 96, 27, 61, 152, 239];
pub const CLAIM_BOOKING_DISCRIMINATOR: [u8; 8] = [143, 218, 32, 94, 7, 208, 186, 252];
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Market,
    Model,
    ModelGroup,
    Provider,
    Metadata,
    Terms,
    Capacity,
    User,
    Booking,
    ProviderList,
    ListMetadata,
    BookingPolicies,
    WalletBookings,
    ProviderPolicy,
    ProviderCapacity,
    ListRouting,
    ListTerms,
    ListCapacity,
    ProviderWallet,
    ProviderLimitPage,
    ProviderLimitIndex,
}

mod accounts;
mod bytes;
mod values;

pub use accounts::*;
pub use bytes::*;
pub use values::*;
