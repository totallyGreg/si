use axum::{
    extract::{OriginalUri, Path},
    response::IntoResponse,
};
use dal::{ChangeSet, ChangeSetId, SchemaVariant, SchemaVariantId, WorkspacePk};

use crate::server::{
    extract::{AccessBuilder, HandlerContext, PosthogClient},
    tracking::track,
};

use super::SchemaVariantsAPIResult;

pub async fn delete_unlocked_variant(
    HandlerContext(builder): HandlerContext,
    AccessBuilder(access_builder): AccessBuilder,
    PosthogClient(posthog_client): PosthogClient,
    OriginalUri(original_uri): OriginalUri,
    Path((_workspace_pk, change_set_id, schema_variant_id)): Path<(
        WorkspacePk,
        ChangeSetId,
        SchemaVariantId,
    )>,
) -> SchemaVariantsAPIResult<impl IntoResponse> {
    let mut ctx = builder
        .build(access_builder.build(change_set_id.into()))
        .await?;
    let force_change_set_id = ChangeSet::force_new(&mut ctx).await?;
    let schema_variant = SchemaVariant::get_by_id_or_error(&ctx, schema_variant_id).await?;

    SchemaVariant::cleanup_unlocked_variant(&ctx, schema_variant_id).await?;

    track(
        &posthog_client,
        &ctx,
        &original_uri,
        "delete_unlocked_variant",
        serde_json::json!({
            "schema_variant_id": schema_variant_id,
            "schema_variant_name": schema_variant.display_name(),
            "schema_variant_version": schema_variant.version(),
        }),
    );
    ctx.commit().await?;

    let mut response = axum::response::Response::builder();
    response = response.header("Content-Type", "application/json");
    if let Some(force_change_set_id) = force_change_set_id {
        response = response.header("force_change_set_id", force_change_set_id.to_string());
    }

    Ok(response.body(serde_json::to_string(&schema_variant_id)?)?)
}
