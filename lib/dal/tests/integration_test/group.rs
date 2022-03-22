use crate::test_setup;

use crate::dal::test;
use dal::test_harness::{
    create_billing_account, create_change_set, create_edit_session, create_group, create_user,
    create_visibility_edit_session,
};
use dal::{Group, HistoryActor, StandardModel, Tenancy, WriteTenancy};

#[test]
async fn new() {
    test_setup!(
        ctx,
        _secret_key,
        pg,
        conn,
        txn,
        nats_conn,
        nats,
        _veritech,
        _encr_key
    );
    let write_tenancy = WriteTenancy::new_universal();
    let history_actor = HistoryActor::SystemInit;
    let change_set = create_change_set(&txn, &nats, &(&write_tenancy).into(), &history_actor).await;
    let edit_session = create_edit_session(&txn, &nats, &history_actor, &change_set).await;
    let visibility = create_visibility_edit_session(&change_set, &edit_session);
    let _group = Group::new(
        &txn,
        &nats,
        &write_tenancy,
        &visibility,
        &history_actor,
        "funky",
    )
    .await
    .expect("cannot create group");
}

#[test]
async fn add_user() {
    test_setup!(
        ctx,
        _secret_key,
        pg,
        conn,
        txn,
        nats_conn,
        nats,
        _veritech,
        _encr_key
    );
    let universal_tenancy = Tenancy::new_universal();
    let history_actor = HistoryActor::SystemInit;
    let change_set = create_change_set(&txn, &nats, &universal_tenancy, &history_actor).await;
    let edit_session = create_edit_session(&txn, &nats, &history_actor, &change_set).await;
    let visibility = create_visibility_edit_session(&change_set, &edit_session);
    let billing_account =
        create_billing_account(&txn, &nats, &universal_tenancy, &visibility, &history_actor).await;
    let tenancy = Tenancy::new_billing_account(vec![*billing_account.id()]);
    let group = create_group(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_one = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_two = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_one.id())
        .await
        .expect("cannot add user");
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_two.id())
        .await
        .expect("cannot add user");
}

#[test]
async fn remove_user() {
    test_setup!(
        ctx,
        _secret_key,
        pg,
        conn,
        txn,
        nats_conn,
        nats,
        _veritech,
        _encr_key
    );
    let universal_tenancy = Tenancy::new_universal();
    let history_actor = HistoryActor::SystemInit;
    let change_set = create_change_set(&txn, &nats, &universal_tenancy, &history_actor).await;
    let edit_session = create_edit_session(&txn, &nats, &history_actor, &change_set).await;
    let visibility = create_visibility_edit_session(&change_set, &edit_session);
    let billing_account =
        create_billing_account(&txn, &nats, &universal_tenancy, &visibility, &history_actor).await;
    let tenancy = Tenancy::new_billing_account(vec![*billing_account.id()]);
    let group = create_group(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_one = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_two = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_one.id())
        .await
        .expect("cannot remove user");
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_two.id())
        .await
        .expect("cannot remove user");
    group
        .remove_user(&txn, &nats, &visibility, &history_actor, user_one.id())
        .await
        .expect("cannot remove user");
    group
        .remove_user(&txn, &nats, &visibility, &history_actor, user_two.id())
        .await
        .expect("cannot remove user");
}

#[test]
async fn users() {
    test_setup!(
        ctx,
        _secret_key,
        pg,
        conn,
        txn,
        nats_conn,
        nats,
        _veritech,
        _encr_key
    );
    let universal_tenancy = Tenancy::new_universal();
    let history_actor = HistoryActor::SystemInit;
    let change_set = create_change_set(&txn, &nats, &universal_tenancy, &history_actor).await;
    let edit_session = create_edit_session(&txn, &nats, &history_actor, &change_set).await;
    let visibility = create_visibility_edit_session(&change_set, &edit_session);
    let billing_account =
        create_billing_account(&txn, &nats, &universal_tenancy, &visibility, &history_actor).await;
    let tenancy = Tenancy::new_billing_account(vec![*billing_account.id()]);
    let group = create_group(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_one = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    let user_two = create_user(&txn, &nats, &tenancy, &visibility, &history_actor).await;
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_one.id())
        .await
        .expect("cannot add user");
    group
        .add_user(&txn, &nats, &visibility, &history_actor, user_two.id())
        .await
        .expect("cannot add user");

    let all_users = group
        .users(&txn, &visibility)
        .await
        .expect("cannot list users for group");
    assert_eq!(
        all_users,
        vec![user_one.clone(), user_two.clone()],
        "all associated users in the list"
    );

    group
        .remove_user(&txn, &nats, &visibility, &history_actor, user_one.id())
        .await
        .expect("cannot remove user");
    let some_users = group
        .users(&txn, &visibility)
        .await
        .expect("cannot list users for group");
    txn.commit().await.expect("cannot commit txn");
    assert_eq!(
        some_users,
        vec![user_two.clone()],
        "only one associated user in the list"
    );
}
