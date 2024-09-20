use std::{collections::HashMap, str::FromStr, sync::Arc};

use chrono::{DateTime, Utc};
use itertools::Itertools;
use postgres_types::ToSql;
use telemetry::prelude::*;
use thiserror::Error;
use tokio::task::JoinSet;
use ulid::Ulid;

use crate::{
    pk,
    slow_rt::{self, SlowRuntimeError},
    ComponentType, DalContext, SchemaId, TransactionsError,
};
use module_index_client::{ModuleDetailsResponse, ModuleIndexClient, ModuleIndexClientError};
use si_data_pg::{PgError, PgRow};
use si_pkg::{SiPkg, SiPkgError};

pk!(CachedModuleId);

#[remain::sorted]
#[derive(Error, Debug)]
pub enum CachedModuleError {
    #[error("join error: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("module index client error: {0}")]
    ModuleIndexClient(#[from] ModuleIndexClientError),
    #[error("No module index url set on the services context")]
    ModuleIndexUrlNotSet,
    #[error("pg error: {0}")]
    Pg(#[from] PgError),
    #[error("si-pkg error: {0}")]
    SiPkg(#[from] SiPkgError),
    #[error("slow runtime error: {0}")]
    SlowRuntime(#[from] SlowRuntimeError),
    #[error("strum parse error: {0}")]
    StrumParse(#[from] strum::ParseError),
    #[error("transactions error: {0}")]
    Transactions(#[from] TransactionsError),
    #[error("url parse error: {0}")]
    UrlParse(#[from] url::ParseError),
}

pub type CachedModuleResult<T> = Result<T, CachedModuleError>;

pub struct CachedModule {
    pub id: CachedModuleId,
    pub schema_id: SchemaId,
    pub schema_name: String,
    pub display_name: Option<String>,
    pub category: Option<String>,
    pub link: Option<String>,
    pub color: Option<String>,
    pub description: Option<String>,
    pub component_type: ComponentType,
    pub latest_hash: String,
    pub created_at: DateTime<Utc>,
    pub package_data: Option<Vec<u8>>,
}

impl TryFrom<PgRow> for CachedModule {
    type Error = CachedModuleError;

    fn try_from(row: PgRow) -> Result<Self, Self::Error> {
        let component_type_string: String = row.try_get("component_type")?;
        let component_type = ComponentType::from_str(&component_type_string)?;

        Ok(Self {
            id: row.try_get("id")?,
            schema_id: row.try_get("schema_id")?,
            schema_name: row.try_get("schema_name")?,
            display_name: row.try_get("display_name")?,
            category: row.try_get("category")?,
            link: row.try_get("link")?,
            color: row.try_get("color")?,
            description: row.try_get("description")?,
            component_type,
            latest_hash: row.try_get("latest_hash")?,
            created_at: row.try_get("created_at")?,
            package_data: Some(row.try_get("package_data")?),
        })
    }
}

impl CachedModule {
    pub async fn find_missing_entries(
        ctx: &DalContext,
        hashes: Vec<String>,
    ) -> CachedModuleResult<Vec<String>> {
        // Constructs a list of parameters like '($1), ($2), ($3), ($4)' for
        // each input value so they can be used as a table expression in the
        // query, for the left join
        let values_expr = hashes
            .iter()
            .enumerate()
            .map(|(idx, _)| format!("(${})", idx + 1))
            .join(",");

        let params: Vec<_> = hashes
            .iter()
            .map(|hash| hash as &(dyn ToSql + Sync))
            .collect();

        let query = format!(
            "
            SELECT hashes.hash 
                FROM (VALUES {values_expr}) AS hashes(hash)
            LEFT JOIN cached_modules on cached_modules.latest_hash = hashes.hash
            WHERE cached_modules.latest_hash IS NULL
            "
        );

        let rows = ctx.txns().await?.pg().query(&query, &params).await?;

        let mut result = vec![];

        for row in rows {
            result.push(row.try_get("hash")?);
        }

        Ok(result)
    }

    /// Calls out to the module index server to fetch the latest module set, and
    /// updates the cache for any new builtin modules
    pub async fn update_cached_modules(ctx: &DalContext) -> CachedModuleResult<()> {
        let services_context = ctx.services_context();
        let module_index_url = services_context
            .module_index_url()
            .ok_or(CachedModuleError::ModuleIndexUrlNotSet)?;

        let module_index_client =
            ModuleIndexClient::unauthenticated_client(module_index_url.try_into()?);

        let modules: HashMap<_, _> = module_index_client
            .list_builtins()
            .await?
            .modules
            .iter()
            .map(|builtin| (builtin.latest_hash.to_owned(), builtin.to_owned()))
            .collect();

        let hashes: Vec<_> = modules.keys().map(ToOwned::to_owned).collect();
        let uncached_hashes = CachedModule::find_missing_entries(&ctx, hashes).await?;

        let mut join_set = JoinSet::new();
        for uncached_hash in &uncached_hashes {
            let Some(module) = modules.get(uncached_hash).cloned() else {
                continue;
            };

            let client = module_index_client.clone();
            join_set.spawn(async move {
                let module_id = module.id.to_owned();
                Ok::<(ModuleDetailsResponse, Arc<Vec<u8>>), CachedModuleError>((
                    module,
                    Arc::new(
                        client
                            .get_builtin(Ulid::from_string(&module_id).unwrap_or_default())
                            .await?,
                    ),
                ))
            });
        }

        for res in join_set.join_all().await {
            match res {
                Ok((module, module_bytes)) => {
                    Self::insert(ctx, &module, module_bytes).await?;
                }
                Err(_) => todo!(),
            }
        }

        if !uncached_hashes.is_empty() {
            ctx.commit_no_rebase().await?;
        }

        Ok(())
    }

    pub async fn latest_modules(ctx: &DalContext) -> CachedModuleResult<Vec<CachedModule>> {
        let query = "
            SELECT DISTINCT ON (schema_id)
                id
                schema_id,
                schema_name,
                display_name,
                category,
                link,
                color,
                description,
                component_type,
                latest_hash,
                created_at
            FROM cached_modules
            ORDER BY schema_id, created_at DESC
        ";

        let rows = ctx.txns().await?.pg().query(&query, &[]).await?;

        let mut result = vec![];

        for row in rows {
            let component_type_string: String = row.try_get("component_type")?;
            let component_type = ComponentType::from_str(&component_type_string)?;

            let cached_module = Self {
                id: row.try_get("id")?,
                schema_id: row.try_get("schema_id")?,
                schema_name: row.try_get("schema_name")?,
                display_name: row.try_get("display_name")?,
                category: row.try_get("category")?,
                link: row.try_get("link")?,
                color: row.try_get("color")?,
                description: row.try_get("description")?,
                component_type,
                latest_hash: row.try_get("latest_hash")?,
                created_at: row.try_get("created_at")?,
                package_data: None,
            };
            result.push(cached_module);
        }

        Ok(result)
    }

    pub async fn insert(
        ctx: &DalContext,
        module_details: &ModuleDetailsResponse,
        pkg_bytes: Arc<Vec<u8>>,
    ) -> CachedModuleResult<Option<Self>> {
        let bytes_clone = pkg_bytes.clone();
        let pkg = slow_rt::spawn(async move { SiPkg::load_from_bytes(&bytes_clone) })?.await??;

        let query = "
            INSERT INTO cached_modules (
                schema_id,
                schema_name,
                display_name,
                category,
                link,
                color,
                description,
                component_type,
                latest_hash,
                created_at,
                package_data
            ) VALUES (
                $1, $2, $3, $4, $5, $6,
                $7, $8, $9, $10, $11 
            ) RETURNING *
        ";

        let Some(schema_id) = module_details.schema_id() else {
            warn!("builtin module {} has no schema id", module_details.id);
            return Ok(None);
        };
        let schema_id: SchemaId = schema_id.into();

        let Some(pkg_schema) = pkg.schemas()?.first().cloned() else {
            warn!("builtin module {} has no schema", module_details.id);
            return Ok(None);
        };

        let Some(pkg_variant) = pkg_schema.variants()?.first().cloned() else {
            warn!(
                "builtin module {} has a schema with no variant",
                module_details.id
            );
            return Ok(None);
        };

        let schema_name = pkg_schema
            .data()
            .map(|data| data.name())
            .unwrap_or(module_details.name.as_str());
        let display_name = pkg_schema.data().and_then(|data| data.category_name());
        let category = pkg_schema.data().map(|data| data.category()).unwrap_or("");
        let link = pkg_variant
            .data()
            .and_then(|data| data.link().map(ToString::to_string));
        let color = pkg_variant.data().and_then(|data| data.color());
        let description = pkg_variant.data().and_then(|data| data.description());
        let component_type: ComponentType = pkg_variant
            .data()
            .map(|data| data.component_type().into())
            .unwrap_or_default();

        info!(
            "Updating sdf module cache for {} - {schema_name} ({category:?})",
            module_details.name
        );

        let bytes_ref = pkg_bytes.as_slice();
        let row = ctx
            .txns()
            .await?
            .pg()
            .query_one(
                query,
                &[
                    &schema_id,
                    &schema_name,
                    &display_name,
                    &category,
                    &link,
                    &color,
                    &description,
                    &component_type.to_string(),
                    &module_details.latest_hash,
                    &module_details.created_at,
                    &bytes_ref,
                ],
            )
            .await?;

        Ok(Some(row.try_into()?))
    }
}
