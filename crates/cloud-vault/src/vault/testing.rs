//! The individual steps of an upload, for tests that interrupt one between
//! them. Production code uses [`Vault::upload`] and [`Vault::settle_pending`].
use super::*;

impl Vault {
    pub fn queue_upload(
        &mut self,
        desired: &Manifest,
        remote: &RemoteView,
        adapter: &str,
        authorization: Authorization,
    ) -> Result<String> {
        self.queue(desired, remote, adapter, authorization)
    }

    pub fn begin_upload(
        &mut self,
        id: &str,
        authorization: Authorization,
        current: &RemoteView,
    ) -> Result<UploadPermit> {
        self.begin(id, authorization, current)
    }

    pub fn complete_upload(
        &mut self,
        permit: &UploadPermit,
        server_confirmed: bool,
        readback: &RemoteView,
    ) -> Result<()> {
        self.complete(permit, server_confirmed, readback)
    }

    pub fn reconcile_pending(
        &mut self,
        id: &str,
        authorization: Authorization,
        transaction_settled: bool,
        readback: &RemoteView,
    ) -> Result<()> {
        self.reconcile(id, authorization, transaction_settled, readback)
    }

    pub fn recompare_pending(
        &mut self,
        id: &str,
        authorization: Authorization,
        observed: &RemoteView,
    ) -> Result<()> {
        self.recompare(id, authorization, observed)
    }

    pub fn resolve_pending_conflict(
        &mut self,
        id: &str,
        shown: &RemoteView,
        current: &RemoteView,
        keep_local: bool,
    ) -> Result<()> {
        self.resolve_conflict(id, shown, current, keep_local)
    }
}
