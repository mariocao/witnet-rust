use super::*;
use witnet_data_structures::chain::{Block, EpochConstants};

pub struct State {
    pub name: Option<String>,
    pub caption: Option<String>,
    pub account: u32,
    pub keychains: [types::ExtendedSK; 2],
    pub next_external_index: u32,
    pub next_internal_index: u32,
    pub available_accounts: Vec<u32>,
    pub balance: u64,
    pub transaction_next_id: u32,
    pub utxo_set: model::UtxoSet,
    pub epoch_constants: EpochConstants,
    /// Last received block to be indexed when next block is received.
    /// Note: the wallet always keeps a buffer of one block without indexing (so as to deal with chain tip reorganizations).
    pub last_block: Option<Block>,
    /// Beacon of the last block received during synchronization.
    pub last_sync: CheckpointBeacon,
}
