use actix::prelude::*;

use crate::actors::worker;
use crate::types;

pub struct NotifyStatus(pub types::SessionWallet, pub types::DynamicSink);

impl Message for NotifyStatus {
    type Result = ();
}

impl Handler<NotifyStatus> for worker::Worker {
    type Result = <NotifyStatus as Message>::Result;

    fn handle(
        &mut self,
        NotifyStatus(wallet, sink): NotifyStatus,
        _ctx: &mut Self::Context,
    ) -> Self::Result {
        if let Err(err) = self.notify_client(&wallet, sink, None) {
            log::warn!(
                "failed to notify wallet {} about its status: {}",
                wallet.id,
                err
            );
        }
    }
}
