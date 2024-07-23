use chrono::Utc;
use dal::module::Module;
use dal::pkg::export::PkgExporter;
use dal::{DalContext, Schema};
use dal_test::test;
use si_pkg::{SocketSpecArity, SocketSpecKind};
use std::collections::HashMap;
use ulid::Ulid;

#[test]
async fn list_modules(ctx: &DalContext) {
    let modules = Module::list_installed(ctx)
        .await
        .expect("unable to get installed modules");

    let mut module_names: Vec<String> = modules.iter().map(|m| m.name().to_string()).collect();
    module_names.sort();

    let expected_installed_module_names = vec![
        "BadValidations".to_string(),
        "ValidatedInput".to_string(),
        "ValidatedOutput".to_string(),
        "dummy-secret".to_string(),
        "etoiles".to_string(),
        "fallout".to_string(),
        "katy perry".to_string(),
        "large even lego".to_string(),
        "large odd lego".to_string(),
        "medium even lego".to_string(),
        "medium odd lego".to_string(),
        "morningstar".to_string(),
        "pet_shop".to_string(),
        "pirate".to_string(),
        "si-coreos-2023-09-13".to_string(),
        "si-docker-image-2023-09-13".to_string(),
        "si-intrinsic-funcs".to_string(),
        "small even lego".to_string(),
        "small odd lego".to_string(),
        "starfield".to_string(),
        "swifty".to_string(),
    ];

    assert_eq!(expected_installed_module_names, module_names);
}

#[test]
async fn get_fallout_module(ctx: &DalContext) {
    let modules = Module::list_installed(ctx)
        .await
        .expect("unable to get installed modules");

    let mut filtered_modules: Vec<Module> = modules
        .into_iter()
        .filter(|m| m.name() == "fallout")
        .collect();

    assert_eq!(1, filtered_modules.len());

    if let Some(fallout_module) = filtered_modules.pop() {
        let associated_funcs = fallout_module
            .list_associated_funcs(ctx)
            .await
            .expect("Unable to get association funcs");
        let associated_schemas = fallout_module
            .list_associated_schemas(ctx)
            .await
            .expect("Unable to get association schemas");

        assert_eq!("fallout", fallout_module.name());
        assert_eq!("System Initiative", fallout_module.created_by_email());
        assert_eq!("2019-06-03", fallout_module.version());
        assert_eq!(4, associated_funcs.len());
        assert_eq!(1, associated_schemas.len());
    }
}

#[test]
async fn module_export_simple(ctx: &mut DalContext) {
    let schema = Schema::find_by_name(ctx, "dummy-secret")
        .await
        .expect("unable to get schema")
        .expect("schema not found");

    let default_schema_variant = schema
        .get_default_schema_variant_id(ctx)
        .await
        .expect("Unable to find the default schema variant id");

    assert!(default_schema_variant.is_some());

    let name = "Paul's Test Pkg".to_string();
    let description = "The Bison".to_string();
    let version = "2019-06-03".to_string();
    let user = "System Initiative".to_string();
    let mut exporter = PkgExporter::new_module_exporter(
        name.clone(),
        version.clone(),
        Some(description.clone()),
        &user,
        vec![schema.id()],
    );

    let exported_pkg = exporter
        .export_as_spec(ctx)
        .await
        .expect("unable to get the pkg spec");

    assert_eq!(exported_pkg.name, name.clone());
    assert_eq!(exported_pkg.description, description.clone());
    assert_eq!(exported_pkg.version, version.clone());
    assert_eq!(exported_pkg.created_by, user.clone());
    assert_eq!(exported_pkg.funcs.len(), 14);

    let pkg_schemas = exported_pkg.clone().schemas;
    assert_eq!(pkg_schemas.len(), 1);

    let pkg_schema = pkg_schemas
        .first()
        .expect("unable to get the package schema");
    assert_eq!(pkg_schema.variants.len(), 1);

    let pkg_schema_spec = pkg_schema.clone().data.expect("unable to get schema spec");
    assert_eq!(pkg_schema_spec.name, "dummy-secret");
    assert_eq!(pkg_schema_spec.category, "test exclusive");

    let pkg_schema_variant = pkg_schema
        .variants
        .first()
        .expect("unable to get the schema variant");
    assert_eq!(pkg_schema_variant.auth_funcs.len(), 1);
    assert_eq!(pkg_schema_variant.leaf_functions.len(), 1);
    assert_eq!(pkg_schema_variant.sockets.len(), 1);
    assert_eq!(pkg_schema_variant.si_prop_funcs.len(), 2);
    assert_eq!(pkg_schema_variant.root_prop_funcs.len(), 1);

    let socket = pkg_schema_variant
        .sockets
        .first()
        .expect("unable to get the socket");
    assert_eq!(socket.name, "dummy");
    assert_eq!(socket.inputs.len(), 1);

    let socket_spec = socket.clone().data.expect("unable to get socket spec data");
    assert_eq!(socket_spec.name, "dummy");
    assert_eq!(socket_spec.arity, SocketSpecArity::One);
    assert_eq!(socket_spec.kind, SocketSpecKind::Output);
    assert_eq!(
        socket_spec.connection_annotations,
        "[{\"tokens\":[\"dummy\"]}]"
    );

    let mut exported_pkg_func_names: Vec<String> =
        exported_pkg.funcs.iter().map(|f| f.name.clone()).collect();
    exported_pkg_func_names.sort();

    let expected_func_names = vec![
        "si:identity".to_string(),
        "si:resourcePayloadToValue".to_string(),
        "si:setArray".to_string(),
        "si:setBoolean".to_string(),
        "si:setInteger".to_string(),
        "si:setJson".to_string(),
        "si:setMap".to_string(),
        "si:setObject".to_string(),
        "si:setString".to_string(),
        "si:unset".to_string(),
        "si:validation".to_string(),
        "test:qualificationDummySecretStringIsTodd".to_string(),
        "test:scaffoldDummySecretAsset".to_string(),
        "test:setDummySecretString".to_string(),
    ];

    assert_eq!(exported_pkg_func_names, expected_func_names);
}

#[test]
async fn dummy_sync(ctx: &DalContext) {
    let schema = Schema::find_by_name(ctx, "starfield")
        .await
        .expect("could not perform find by name")
        .expect("schema not found");
    let schema_variant_id = schema
        .get_default_schema_variant_id(ctx)
        .await
        .expect("could not perform get default schema variant id")
        .expect("no schema variant id found");
    let module = Module::find_for_member_id(ctx, schema.id())
        .await
        .expect("could not perform find for module schema id")
        .expect("module not found");

    // Create dummy latest modules.
    let now = Utc::now();
    let dummy_latest_module_upgradeable = si_frontend_types::LatestModule {
        id: Ulid::new().to_string(),
        name: module.name().to_owned(),
        description: Some(module.description().to_owned()),
        owner_user_id: "this is BS!".to_string(),
        owner_display_name: None,
        metadata: serde_json::Value::Null,
        latest_hash: Ulid::new().to_string(),
        latest_hash_created_at: now,
        created_at: now,
        schema_id: Some(schema.id().into()),
    };
    let dummy_latest_module_installable = si_frontend_types::LatestModule {
        id: Ulid::new().to_string(),
        name: "threadripper 7980X".to_string(),
        description: None,
        owner_user_id: "this is also BS!".to_string(),
        owner_display_name: None,
        metadata: serde_json::Value::Null,
        latest_hash: Ulid::new().to_string(),
        latest_hash_created_at: now,
        created_at: now,
        schema_id: Some(Ulid::new().to_string()),
    };

    // Assemble our expected result.
    let mut expected = si_frontend_types::SyncedModules::new();
    expected.upgradeable.insert(
        schema_variant_id.into(),
        dummy_latest_module_upgradeable.clone(),
    );
    expected
        .installable
        .push(dummy_latest_module_installable.clone());

    // Perform the sync and check that the result is what we expect.
    let actual = Module::sync(
        ctx,
        vec![
            dummy_latest_module_upgradeable,
            dummy_latest_module_installable,
        ],
        HashMap::new(),
    )
    .await
    .expect("could not sync");
    assert_eq!(
        expected, // expected
        actual    // actual
    );
}
