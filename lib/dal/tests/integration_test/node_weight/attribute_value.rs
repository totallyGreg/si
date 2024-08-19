use dal::DalContext;
use dal_test::expected::{
    apply_change_set_to_base, commit_and_update_snapshot_to_visibility, fork_from_head_change_set,
    update_visibility_and_snapshot_to_visibility, ExpectComponent,
};
use dal_test::helpers::connect_components_with_socket_names;
use dal_test::test;

#[test]
async fn change_in_output_component_produces_dvu_root_in_other_change_set(ctx: &mut DalContext) {
    let docker_image = ExpectComponent::create(ctx, "Docker Image").await;

    apply_change_set_to_base(ctx).await;

    let cs_with_butane = fork_from_head_change_set(ctx).await;
    let butane = ExpectComponent::create(ctx, "Butane").await;
    connect_components_with_socket_names(
        ctx,
        docker_image.id(),
        "Container Image",
        butane.id(),
        "Container Image",
    )
    .await
    .expect("able to connect");

    commit_and_update_snapshot_to_visibility(ctx).await;
    fork_from_head_change_set(ctx).await;

    let image = docker_image.prop(ctx, ["root", "domain", "image"]).await;
    image.set(ctx, "unpossible").await;

    apply_change_set_to_base(ctx).await;

    update_visibility_and_snapshot_to_visibility(ctx, cs_with_butane.id).await;

    assert_eq!(serde_json::json!("unpossible"), image.get(ctx).await);

    // DVU debouncer does not run in tests so these roots will never get
    // processed unless we explicitly enqueue a dvu. It's enough to see that it
    // made it into the roots
    assert!(ctx
        .workspace_snapshot()
        .expect("get snap")
        .list_dependent_value_value_ids()
        .await
        .expect("able to get dvu values")
        .contains(&image.attribute_value(ctx).await.id().into()));
}