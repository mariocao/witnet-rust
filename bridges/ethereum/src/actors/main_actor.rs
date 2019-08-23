//! Actor which receives Witnet blocks and sends proofs of inclusion to Ethereum

use crate::{actors::handle_receipt, actors::ActorMessage, config::Config, eth::EthState};
use ethabi::Bytes;
use futures::{future::Either, sink::Sink, stream::Stream};
use log::*;
use std::sync::Arc;
use tokio::{sync::mpsc, sync::oneshot};
use web3::{contract, futures::Future, types::U256};
use witnet_data_structures::{
    chain::{Hash, Hashable},
    proto::ProtobufConvert,
};

/// Actor which receives Witnet blocks and sends proofs of inclusion to Ethereum
pub fn main_actor(
    config: Arc<Config>,
    eth_state: Arc<EthState>,
    wait_for_witnet_block_tx: mpsc::Sender<(U256, oneshot::Sender<()>)>,
) -> (
    mpsc::Sender<ActorMessage>,
    impl Future<Item = (), Error = ()>,
) {
    let (tx, rx) = mpsc::channel(16);

    let fut = rx.map_err(|_| ())
        .for_each(move |msg| {
            debug!("Got ActorMessage: {:?}", msg);
            let eth_state = eth_state.clone();
            let eth_state2 = eth_state.clone();
            let eth_account = config.eth_account;
            let wbi_contract = eth_state.wbi_contract.clone();
            let block_relay_contract = eth_state.block_relay_contract.clone();

            let (block, is_new_block) = match msg {
                ActorMessage::NewWitnetBlock(block) => (block, true),
                ActorMessage::ReplayWitnetBlock(block) => (block, false),
            };

            // Optimization: do not process empty blocks
            let empty_hash = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".parse().unwrap();
            if block.block_header.merkle_roots.dr_hash_merkle_root == empty_hash && block.block_header.merkle_roots.tally_hash_merkle_root == empty_hash {
                debug!("Skipping empty block");
                return futures::finished(());
            }

            let block_hash: U256 = match block.hash() {
                Hash::SHA256(x) => x.into(),
            };

            // Enable block relay?
            if is_new_block && config.enable_block_relay {
                let block_epoch: U256 = block.block_header.beacon.checkpoint.into();
                let dr_merkle_root: U256 =
                    match block.block_header.merkle_roots.dr_hash_merkle_root {
                        Hash::SHA256(x) => x.into(),
                    };
                let tally_merkle_root: U256 =
                    match block.block_header.merkle_roots.tally_hash_merkle_root {
                        Hash::SHA256(x) => x.into(),
                    };

                let block_relay_contract2 = block_relay_contract.clone();

                // Post witnet block to BlockRelay wbi_contract
                tokio::spawn(
                    block_relay_contract
                        .query(
                            "readDrMerkleRoot",
                            (block_hash,),
                            eth_account,
                            contract::Options::default(),
                            None,
                        )
                        .map(move |_: U256| {
                            debug!("Block {:x} was already posted", block_hash);
                        })
                        .or_else(move |_| {
                            debug!("Trying to relay block {:x}", block_hash);
                            block_relay_contract2
                                .call_with_confirmations(
                                    "postNewBlock",
                                    (block_hash, block_epoch, dr_merkle_root, tally_merkle_root),
                                    eth_account,
                                    contract::Options::with(|opt| {
                                        opt.gas = Some(100_000.into());
                                    }),
                                    1,
                                )
                                .map_err(|e| error!("postNewBlock: {:?}", e))
                                .and_then(|tx| {
                                    debug!("postNewBlock: {:?}", tx);
                                    handle_receipt(tx)
                                })
                                .map(move |()| {
                                    info!("Posted block {:x} to block relay", block_hash);
                                })
                                .map_err(move |()| {
                                    warn!("Failed to post block {:x} to block relay, maybe it was already posted?", block_hash);
                                })
                        })
                );
            }

            // Wait for someone else to publish the witnet block to ethereum
            let (wbtx, wbrx) = oneshot::channel();
            let fut = wait_for_witnet_block_tx.clone().send((block_hash, wbtx)).map_err(|_| ())
                .and_then(|_| {
                    wbrx.map_err(|_| ())
                })
                .and_then(move |()| {
                    eth_state.wbi_requests.read()
                })
                .and_then(move |wbi_requests| {
                    let block_hash: U256 = match block.hash() {
                        Hash::SHA256(x) => x.into(),
                    };

                    let mut including = vec![];
                    let mut resolving = vec![];

                    let claimed_drs = wbi_requests.claimed();
                    let waiting_for_tally = wbi_requests.included();

                    for dr in &block.txns.data_request_txns {
                        if let Some(dr_id) =
                        claimed_drs.get_by_right(&dr.body.dr_output.hash())
                        {
                            let dr_inclusion_proof = match dr.data_proof_of_inclusion(&block) {
                                Some(x) => x,
                                None => {
                                    error!("Error creating data request proof of inclusion");
                                    continue;
                                }
                            };

                            let dr_bytes = match dr.body.dr_output.to_pb_bytes() {
                                Ok(x) => x,
                                Err(e) => {
                                    error!("Error serializing data request output to Protocol Buffers: {:?}", e);
                                    continue;
                                }
                            };

                            debug!(
                                "Proof of inclusion for data request {}:\nData: {:?}\n{:?}",
                                dr.hash(),
                                dr_bytes,
                                dr_inclusion_proof
                            );
                            info!("[{}] Claimed dr got included in witnet block!", dr_id);
                            info!("[{}] Sending proof of inclusion to WBI wbi_contract", dr_id);

                            let poi: Vec<U256> = dr_inclusion_proof
                                .lemma
                                .iter()
                                .map(|x| match x {
                                    Hash::SHA256(x) => x.into(),
                                })
                                .collect();
                            let poi_index = U256::from(dr_inclusion_proof.index);
                            including.push((*dr_id, poi.clone(), poi_index, block_hash));
                            tokio::spawn(
                                wbi_contract
                                    .call_with_confirmations(
                                        "reportDataRequestInclusion",
                                        (*dr_id, poi, poi_index, block_hash),
                                        eth_account,
                                        contract::Options::default(),
                                        1,
                                    )
                                    .map_err(|e| error!("reportDataRequestInclusion: {:?}", e))
                                    .and_then(move |tx| {
                                        debug!("reportDataRequestInclusion: {:?}", tx);
                                        handle_receipt(tx)
                                    }),
                            );
                        }
                    }

                    for tally in &block.txns.tally_txns {
                        if let Some(dr_id) = waiting_for_tally.get_by_right(&tally.dr_pointer)
                        {
                            let Hash::SHA256(dr_pointer_bytes) = tally.dr_pointer;
                            info!("[{}] Found tally for data request, posting to WBI", dr_id);
                            let tally_inclusion_proof = match tally.data_proof_of_inclusion(&block) {
                                Some(x) => x,
                                None => {
                                    error!("Error creating tally data proof of inclusion");
                                    continue;
                                }
                            };
                            debug!(
                                "Proof of inclusion for tally        {}:\nData: {:?}\n{:?}",
                                tally.hash(),
                                [&dr_pointer_bytes[..], &tally.tally].concat(),
                                tally_inclusion_proof
                            );

                            // Call report_result
                            let poi: Vec<U256> = tally_inclusion_proof
                                .lemma
                                .iter()
                                .map(|x| match x {
                                    Hash::SHA256(x) => x.into(),
                                })
                                .collect();
                            let poi_index = U256::from(tally_inclusion_proof.index);
                            let result: Bytes = tally.tally.clone();
                            resolving.push((*dr_id, poi.clone(), poi_index, block_hash, result.clone()));
                            tokio::spawn(
                                wbi_contract
                                    .call_with_confirmations(
                                        "reportResult",
                                        (*dr_id, poi, poi_index, block_hash, result),
                                        eth_account,
                                        contract::Options::default(),
                                        1,
                                    )
                                    .map_err(|e| error!("reportResult: {:?}", e))
                                    .and_then(|tx| {
                                        debug!("reportResult: {:?}", tx);
                                        handle_receipt(tx)
                                    }),
                            );
                        }
                    }

                    // Check if we need to acquire a write lock
                    if !including.is_empty() || !resolving.is_empty() {
                        Either::A(eth_state2.wbi_requests.write().map(|mut wbi_requests| {
                            for (dr_id, poi, poi_index, block_hash) in including {
                                wbi_requests.set_including(dr_id, poi, poi_index, block_hash);
                            }
                            for (dr_id, poi, poi_index, block_hash, result) in resolving {
                                wbi_requests.set_resolving(dr_id, poi, poi_index, block_hash, result);
                            }
                        }))
                    } else {
                        Either::B(futures::finished(()))
                    }
                })
                // Without this line the actor will panic on the first failure
                .then(|_| Result::<(), ()>::Ok(()));

            // Process multiple blocks in parallel
            tokio::spawn(fut);

            futures::finished(())
        })
        .map(|_| ());

    (tx, fut)
}