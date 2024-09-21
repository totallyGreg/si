use dal::{
    change_set::{ChangeSet, ChangeSetError, ChangeSetId},
    workspace_snapshot::WorkspaceSnapshotError,
    DalContext, TransactionsError, Workspace, WorkspaceError, WorkspacePk, WorkspaceSnapshot,
    WsEvent, WsEventError,
};
use rebaser_core::api_types::{
    enqueue_updates_request::EnqueueUpdatesRequest, enqueue_updates_response::v1::RebaseStatus,
};
use si_events::{rebase_batch_address::RebaseBatchAddress, WorkspaceSnapshotAddress};
use si_layer_cache::LayerDbError;
use telemetry::prelude::*;
use thiserror::Error;
use tokio::time::Instant;

#[remain::sorted]
#[derive(Debug, Error)]
pub(crate) enum RebaseError {
    #[error("workspace snapshot error: {0}")]
    ChangeSet(#[from] ChangeSetError),
    #[error("layerdb error: {0}")]
    LayerDb(#[from] LayerDbError),
    #[error("missing change set")]
    MissingChangeSet(ChangeSetId),
    #[error("missing rebase batch {0}")]
    MissingRebaseBatch(RebaseBatchAddress),
    #[error("transactions error: {0}")]
    Transactions(#[from] TransactionsError),
    #[error("workspace error: {0}")]
    Workspace(#[from] WorkspaceError),
    #[error("workspace {0} missing")]
    WorkspaceMissing(WorkspacePk),
    #[error("workspace pk expected but was none")]
    WorkspacePkExpected,
    #[error("workspace snapshot error: {0}")]
    WorkspaceSnapshot(#[from] WorkspaceSnapshotError),
    #[error("ws event error: {0}")]
    WsEvent(#[from] WsEventError),
}

type RebaseResult<T> = Result<T, RebaseError>;

#[instrument(
    name = "rebase.perform_rebase",
    level = "info",
    skip_all,
    fields(
        si.change_set.id = %request.change_set_id,
        si.conflicts = Empty,
        si.conflicts.count = Empty,
        si.updates = Empty,
        si.updates.count = Empty,
        si.workspace.id = %request.workspace_id,
    ))]
pub async fn perform_rebase(
    ctx: &mut DalContext,
    request: &EnqueueUpdatesRequest,
) -> RebaseResult<RebaseStatus> {
    let span = Span::current();

    let start = Instant::now();
    let workspace = get_workspace(ctx).await?;
    let updating_head = request.change_set_id == workspace.default_change_set_id().into();

    // Gather everything we need to detect conflicts and updates from the inbound message.
    let mut to_rebase_change_set = ChangeSet::find(ctx, request.change_set_id.into())
        .await?
        .ok_or(RebaseError::MissingChangeSet(request.change_set_id.into()))?;
    let to_rebase_workspace_snapshot_address = to_rebase_change_set.workspace_snapshot_address;
    debug!("before snapshot fetch and parse: {:?}", start.elapsed());
    let to_rebase_workspace_snapshot =
        WorkspaceSnapshot::find(ctx, to_rebase_workspace_snapshot_address).await?;

    let rebase_batch = ctx
        .layer_db()
        .rebase_batch()
        .read_wait_for_memory(&request.updates_address)
        .await?
        .ok_or(RebaseError::MissingRebaseBatch(request.updates_address))?;

    debug!(
        to_rebase_workspace_snapshot_address = %to_rebase_workspace_snapshot_address,
        updates_address = %request.updates_address,
    );
    debug!("after snapshot fetch and parse: {:?}", start.elapsed());

    let corrected_updates = to_rebase_workspace_snapshot
        .correct_transforms(
            rebase_batch.updates().to_vec(),
            !updating_head
                && request
                    .from_change_set_id
                    .is_some_and(|from_id| from_id != to_rebase_change_set.id.into()),
        )
        .await?;
    debug!("corrected transforms: {:?}", start.elapsed());

    to_rebase_workspace_snapshot
        .perform_updates(&corrected_updates)
        .await?;

    debug!("updates complete: {:?}", start.elapsed());

    if !corrected_updates.is_empty() {
        // Once all updates have been performed, we can write out, mark everything as recently seen
        // and update the pointer.
        to_rebase_workspace_snapshot.write(ctx).await?;
        debug!("snapshot written: {:?}", start.elapsed());
        to_rebase_change_set
            .update_pointer(ctx, to_rebase_workspace_snapshot.id().await)
            .await?;

        debug!("pointer updated: {:?}", start.elapsed());

        ctx.set_workspace_snapshot(to_rebase_workspace_snapshot);
    }
    let updates_count = rebase_batch.updates().len();
    span.record("si.updates.count", updates_count.to_string());

    info!("rebase performed: {:?}", start.elapsed());

    // Before replying to the requester, we must commit.
    ctx.commit_no_rebase().await?;

    {
        let ictx = ctx.clone();
        // TODO(fnichol): this spawn needs to be tracked or it will be trivially cancelled on shut
        // down
        tokio::spawn(async move {
            if let Err(error) =
                evict_unused_snapshots(&ictx, &to_rebase_workspace_snapshot_address).await
            {
                error!(?error, "Eviction error: {:?}", error);
            }
            // TODO: RebaseBatch eviction?
        });
    }

    if updating_head && *workspace.pk() != WorkspacePk::NONE {
        let all_open_change_sets = ChangeSet::list_open(ctx).await?;
        for target_change_set in all_open_change_sets.into_iter().filter(|cs| {
            cs.id != workspace.default_change_set_id()
                && cs.id != to_rebase_change_set.id
                && request.from_change_set_id != Some(cs.id.into())
        }) {
            let ctx_clone = ctx.clone();
            let workspace_pk = *workspace.pk();
            let updates_address = request.updates_address;
            // TODO(fnichol): this spawn needs to be tracked or it will be trivially cancelled
            // on shut down
            tokio::task::spawn(async move {
                debug!(
                    "replaying batch {} onto {} from {}",
                    updates_address, target_change_set.id, to_rebase_change_set.id
                );

                if let Err(err) = replay_changes(
                    &ctx_clone,
                    workspace_pk,
                    target_change_set.id,
                    updates_address,
                    to_rebase_change_set.id,
                )
                .await
                {
                    error!(
                        err = ?err,
                        "error replaying rebase batch {} changes onto {}",
                        updates_address,
                        target_change_set.id
                    );
                }
            });
        }
    }

    if !updating_head {
        if let Some(source_change_set_id) = request.from_change_set_id {
            let mut event = WsEvent::change_set_applied(
                ctx,
                source_change_set_id.into(),
                request.change_set_id.into(),
                None,
            )
            .await?;
            event.set_workspace_pk(request.workspace_id.into());
            event.set_change_set_id(Some(request.change_set_id.into()));
            event.publish_immediately(ctx).await?;
        }
    }

    Ok(RebaseStatus::Success {
        updates_performed: request.updates_address,
    })
}

pub(crate) async fn evict_unused_snapshots(
    ctx: &DalContext,
    workspace_snapshot_address: &WorkspaceSnapshotAddress,
) -> RebaseResult<()> {
    if !ChangeSet::workspace_snapshot_address_in_use(ctx, workspace_snapshot_address).await? {
        ctx.layer_db()
            .workspace_snapshot()
            .evict(
                workspace_snapshot_address,
                ctx.events_tenancy(),
                ctx.events_actor(),
            )
            .await?;
    }
    Ok(())
}

async fn replay_changes(
    ctx: &DalContext,
    workspace_pk: WorkspacePk,
    change_set_id: ChangeSetId,
    updates_address: RebaseBatchAddress,
    from_change_set_id: ChangeSetId,
) -> RebaseResult<()> {
    ctx.run_async_rebase_from_change_set(
        workspace_pk,
        change_set_id,
        updates_address,
        from_change_set_id,
    )
    .await?;

    Ok(())
}

async fn get_workspace(ctx: &DalContext) -> RebaseResult<Workspace> {
    let workspace_pk = ctx
        .tenancy()
        .workspace_pk_opt()
        .ok_or(RebaseError::WorkspacePkExpected)?;

    Workspace::get_by_pk(ctx, &workspace_pk)
        .await?
        .ok_or(RebaseError::WorkspaceMissing(workspace_pk))
}
